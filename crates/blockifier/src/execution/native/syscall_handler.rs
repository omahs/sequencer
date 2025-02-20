use std::collections::HashSet;
use std::hash::RandomState;
use std::sync::Arc;

use cairo_native::starknet::{
    BlockInfo,
    ExecutionInfo,
    ExecutionInfoV2,
    Secp256k1Point,
    Secp256r1Point,
    StarknetSyscallHandler,
    SyscallResult,
    TxInfo,
    TxV2Info,
    U256,
};
use cairo_vm::vm::runners::cairo_runner::ExecutionResources;
use starknet_api::contract_class::EntryPointType;
use starknet_api::core::{
    calculate_contract_address,
    ClassHash,
    ContractAddress,
    EntryPointSelector,
    EthAddress,
};
use starknet_api::state::StorageKey;
use starknet_api::transaction::fields::{Calldata, ContractAddressSalt};
use starknet_api::transaction::{EventContent, EventData, EventKey, L2ToL1Payload};
use starknet_types_core::felt::Felt;

use crate::abi::constants;
use crate::execution::call_info::{
    CallInfo,
    MessageToL1,
    OrderedEvent,
    OrderedL2ToL1Message,
    Retdata,
};
use crate::execution::common_hints::ExecutionMode;
use crate::execution::contract_class::RunnableContractClass;
use crate::execution::entry_point::{
    CallEntryPoint,
    CallType,
    ConstructorContext,
    EntryPointExecutionContext,
};
use crate::execution::errors::EntryPointExecutionError;
use crate::execution::execution_utils::execute_deployment;
use crate::execution::native::utils::{calculate_resource_bounds, default_tx_v2_info};
use crate::execution::syscalls::exceeds_event_size_limit;
use crate::execution::syscalls::hint_processor::{
    SyscallExecutionError,
    BLOCK_NUMBER_OUT_OF_RANGE_ERROR,
    INVALID_INPUT_LENGTH_ERROR,
    OUT_OF_GAS_ERROR,
};
use crate::state::state_api::State;
use crate::transaction::objects::TransactionInfo;

pub struct NativeSyscallHandler<'state> {
    // Input for execution.
    pub state: &'state mut dyn State,
    pub resources: &'state mut ExecutionResources,
    pub context: &'state mut EntryPointExecutionContext,
    pub call: CallEntryPoint,

    // Execution results.
    pub events: Vec<OrderedEvent>,
    pub l2_to_l1_messages: Vec<OrderedL2ToL1Message>,
    pub inner_calls: Vec<CallInfo>,

    // Additional information gathered during execution.
    pub read_values: Vec<Felt>,
    pub accessed_keys: HashSet<StorageKey, RandomState>,

    // It is set if an unrecoverable error happens during syscall execution
    pub unrecoverable_error: Option<SyscallExecutionError>,
}

impl<'state> NativeSyscallHandler<'state> {
    pub fn new(
        call: CallEntryPoint,
        state: &'state mut dyn State,
        resources: &'state mut ExecutionResources,
        context: &'state mut EntryPointExecutionContext,
    ) -> NativeSyscallHandler<'state> {
        NativeSyscallHandler {
            state,
            call,
            resources,
            context,
            events: Vec::new(),
            l2_to_l1_messages: Vec::new(),
            inner_calls: Vec::new(),
            read_values: Vec::new(),
            accessed_keys: HashSet::new(),
            unrecoverable_error: None,
        }
    }

    fn execute_inner_call(
        &mut self,
        entry_point: CallEntryPoint,
        remaining_gas: &mut u128,
    ) -> SyscallResult<Retdata> {
        let mut remaining_gas_u64 =
            u64::try_from(*remaining_gas).expect("Failed to convert gas to u64.");
        let call_info = entry_point
            .execute(self.state, self.resources, self.context, &mut remaining_gas_u64)
            .map_err(|e| self.handle_error(remaining_gas, e.into()))?;
        let retdata = call_info.execution.retdata.clone();

        if call_info.execution.failed {
            let error = SyscallExecutionError::SyscallError { error_data: retdata.0 };
            return Err(self.handle_error(remaining_gas, error));
        }

        // TODO(Noa, 1/11/2024): remove this once the gas type is u64.
        // Change the remaining gas value.
        *remaining_gas = u128::from(remaining_gas_u64);

        self.inner_calls.push(call_info);

        Ok(retdata)
    }

    /// Handles all gas-related logics and perform additional checks. In native,
    /// we need to explicitly call this method at the beginning of each syscall.
    fn pre_execute_syscall(
        &mut self,
        remaining_gas: &mut u128,
        syscall_gas_cost: u64,
    ) -> SyscallResult<()> {
        if self.unrecoverable_error.is_some() {
            // An unrecoverable error was found in a previous syscall, we return immediatly to
            // accelerate the end of the execution. The returned data is not important
            return Err(vec![]);
        }
        // Refund `SYSCALL_BASE_GAS_COST` as it was pre-charged.
        let required_gas =
            u128::from(syscall_gas_cost - self.context.gas_costs().syscall_base_gas_cost);

        if *remaining_gas < required_gas {
            // Out of gas failure.
            return Err(vec![
                Felt::from_hex(OUT_OF_GAS_ERROR)
                    .expect("Failed to parse OUT_OF_GAS_ERROR hex string"),
            ]);
        }

        *remaining_gas -= required_gas;

        Ok(())
    }

    fn handle_error(
        &mut self,
        remaining_gas: &mut u128,
        error: SyscallExecutionError,
    ) -> Vec<Felt> {
        // In case of more than one inner call and because each inner call has their own
        // syscall handler, if there is an unrecoverable error at call `n` it will create a
        // `NativeExecutionError`. When rolling back, each call from `n-1` to `1` will also
        // store the result of a previous `NativeExecutionError` in a `NativeExecutionError`
        // creating multiple wraps around the same error. This function is meant to prevent that.
        fn unwrap_native_error(error: SyscallExecutionError) -> SyscallExecutionError {
            match error {
                SyscallExecutionError::EntryPointExecutionError(
                    EntryPointExecutionError::NativeUnrecoverableError(e),
                ) => *e,
                _ => error,
            }
        }

        match error {
            SyscallExecutionError::SyscallError { error_data } => error_data,
            error => {
                assert!(
                    self.unrecoverable_error.is_none(),
                    "Trying to set an unrecoverable error twice in Native Syscall Handler"
                );
                self.unrecoverable_error = Some(unwrap_native_error(error));
                *remaining_gas = 0;
                vec![]
            }
        }
    }

    fn get_tx_info_v1(&self) -> TxInfo {
        let tx_info = &self.context.tx_context.tx_info;
        TxInfo {
            version: tx_info.version().0,
            account_contract_address: Felt::from(tx_info.sender_address()),
            max_fee: tx_info.max_fee_for_execution_info_syscall().0,
            signature: tx_info.signature().0,
            transaction_hash: tx_info.transaction_hash().0,
            chain_id: Felt::from_hex(
                &self.context.tx_context.block_context.chain_info.chain_id.as_hex(),
            )
            .expect("Failed to convert the chain_id to hex."),
            nonce: tx_info.nonce().0,
        }
    }

    fn get_block_info(&self) -> BlockInfo {
        let block_info = &self.context.tx_context.block_context.block_info;
        if self.context.execution_mode == ExecutionMode::Validate {
            let versioned_constants = self.context.versioned_constants();
            let block_number = block_info.block_number.0;
            let block_timestamp = block_info.block_timestamp.0;
            // Round down to the nearest multiple of validate_block_number_rounding.
            let validate_block_number_rounding =
                versioned_constants.get_validate_block_number_rounding();
            let rounded_block_number =
                (block_number / validate_block_number_rounding) * validate_block_number_rounding;
            // Round down to the nearest multiple of validate_timestamp_rounding.
            let validate_timestamp_rounding = versioned_constants.get_validate_timestamp_rounding();
            let rounded_timestamp =
                (block_timestamp / validate_timestamp_rounding) * validate_timestamp_rounding;
            BlockInfo {
                block_number: rounded_block_number,
                block_timestamp: rounded_timestamp,
                sequencer_address: Felt::ZERO,
            }
        } else {
            BlockInfo {
                block_number: block_info.block_number.0,
                block_timestamp: block_info.block_timestamp.0,
                sequencer_address: Felt::from(block_info.sequencer_address),
            }
        }
    }

    fn get_tx_info_v2(&self) -> SyscallResult<TxV2Info> {
        let tx_info = &self.context.tx_context.tx_info;
        let native_tx_info = TxV2Info {
            version: tx_info.version().0,
            account_contract_address: Felt::from(tx_info.sender_address()),
            max_fee: tx_info.max_fee_for_execution_info_syscall().0,
            signature: tx_info.signature().0,
            transaction_hash: tx_info.transaction_hash().0,
            chain_id: Felt::from_hex(
                &self.context.tx_context.block_context.chain_info.chain_id.as_hex(),
            )
            .expect("Failed to convert the chain_id to hex."),
            nonce: tx_info.nonce().0,
            ..default_tx_v2_info()
        };

        match tx_info {
            TransactionInfo::Deprecated(_) => Ok(native_tx_info),
            TransactionInfo::Current(context) => Ok(TxV2Info {
                resource_bounds: calculate_resource_bounds(context)?,
                tip: context.tip.0.into(),
                paymaster_data: context.paymaster_data.0.clone(),
                nonce_data_availability_mode: context.nonce_data_availability_mode.into(),
                fee_data_availability_mode: context.fee_data_availability_mode.into(),
                account_deployment_data: context.account_deployment_data.0.clone(),
                ..native_tx_info
            }),
        }
    }
}

impl<'state> StarknetSyscallHandler for &mut NativeSyscallHandler<'state> {
    fn get_block_hash(
        &mut self,
        block_number: u64,
        remaining_gas: &mut u128,
    ) -> SyscallResult<Felt> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().get_block_hash_gas_cost)?;

        if self.context.execution_mode == ExecutionMode::Validate {
            let err = SyscallExecutionError::InvalidSyscallInExecutionMode {
                syscall_name: "get_block_hash".to_string(),
                execution_mode: ExecutionMode::Validate,
            };
            return Err(self.handle_error(remaining_gas, err));
        }

        let current_block_number =
            self.context.tx_context.block_context.block_info().block_number.0;
        if current_block_number < constants::STORED_BLOCK_HASH_BUFFER
            || block_number > current_block_number - constants::STORED_BLOCK_HASH_BUFFER
        {
            // `panic` is unreachable in this case, also this is covered by tests so we can safely
            // unwrap
            let out_of_range_felt = Felt::from_hex(BLOCK_NUMBER_OUT_OF_RANGE_ERROR)
                .expect("Converting BLOCK_NUMBER_OUT_OF_RANGE_ERROR to Felt should not fail.");
            let error = SyscallExecutionError::SyscallError { error_data: vec![out_of_range_felt] };
            return Err(self.handle_error(remaining_gas, error));
        }

        let key = StorageKey::try_from(Felt::from(block_number))
            .map_err(|e| self.handle_error(remaining_gas, e.into()))?;
        let block_hash_contract_address =
            ContractAddress::try_from(Felt::from(constants::BLOCK_HASH_CONTRACT_ADDRESS))
                .map_err(|e| self.handle_error(remaining_gas, e.into()))?;

        match self.state.get_storage_at(block_hash_contract_address, key) {
            Ok(value) => Ok(value),
            Err(e) => Err(self.handle_error(remaining_gas, e.into())),
        }
    }

    fn get_execution_info(&mut self, remaining_gas: &mut u128) -> SyscallResult<ExecutionInfo> {
        self.pre_execute_syscall(
            remaining_gas,
            self.context.gas_costs().get_execution_info_gas_cost,
        )?;

        Ok(ExecutionInfo {
            block_info: self.get_block_info(),
            tx_info: self.get_tx_info_v1(),
            caller_address: Felt::from(self.call.caller_address),
            contract_address: Felt::from(self.call.storage_address),
            entry_point_selector: self.call.entry_point_selector.0,
        })
    }

    fn get_execution_info_v2(
        &mut self,
        remaining_gas: &mut u128,
    ) -> SyscallResult<ExecutionInfoV2> {
        self.pre_execute_syscall(
            remaining_gas,
            self.context.gas_costs().get_execution_info_gas_cost,
        )?;

        Ok(ExecutionInfoV2 {
            block_info: self.get_block_info(),
            tx_info: self.get_tx_info_v2()?,
            caller_address: Felt::from(self.call.caller_address),
            contract_address: Felt::from(self.call.storage_address),
            entry_point_selector: self.call.entry_point_selector.0,
        })
    }

    fn deploy(
        &mut self,
        class_hash: Felt,
        contract_address_salt: Felt,
        calldata: &[Felt],
        deploy_from_zero: bool,
        remaining_gas: &mut u128,
    ) -> SyscallResult<(Felt, Vec<Felt>)> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().deploy_gas_cost)?;

        let deployer_address = self.call.storage_address;
        let deployer_address_for_calculation =
            if deploy_from_zero { ContractAddress::default() } else { deployer_address };

        let class_hash = ClassHash(class_hash);
        let calldata = Calldata(Arc::new(calldata.to_vec()));

        let deployed_contract_address = calculate_contract_address(
            ContractAddressSalt(contract_address_salt),
            class_hash,
            &calldata,
            deployer_address_for_calculation,
        )
        .map_err(|err| self.handle_error(remaining_gas, err.into()))?;

        let ctor_context = ConstructorContext {
            class_hash,
            code_address: Some(deployed_contract_address),
            storage_address: deployed_contract_address,
            caller_address: deployer_address,
        };

        let mut remaining_gas_u64 =
            u64::try_from(*remaining_gas).expect("Failed to convert gas to u64.");

        let call_info = execute_deployment(
            self.state,
            self.resources,
            self.context,
            ctor_context,
            calldata,
            // Warning: converting of reference would create a new reference to different data,
            // example:
            //     let mut a: u128 = 1;
            //     let a_ref: &mut u128 = &mut a;
            //
            //     let mut b: u64 = u64::try_from(*a_ref).unwrap();
            //
            //     assert_eq!(b, 1);
            //
            //     b += 1;
            //
            //     assert_eq!(b, 2);
            //     assert_eq!(a, 1);
            &mut remaining_gas_u64,
        )
        .map_err(|err| self.handle_error(remaining_gas, err.into()))?;

        *remaining_gas = u128::from(remaining_gas_u64);

        let constructor_retdata = call_info.execution.retdata.0[..].to_vec();

        self.inner_calls.push(call_info);

        Ok((Felt::from(deployed_contract_address), constructor_retdata))
    }
    fn replace_class(&mut self, class_hash: Felt, remaining_gas: &mut u128) -> SyscallResult<()> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().replace_class_gas_cost)?;

        let class_hash = ClassHash(class_hash);
        let contract_class = self
            .state
            .get_compiled_contract_class(class_hash)
            .map_err(|e| self.handle_error(remaining_gas, e.into()))?;

        match contract_class {
            RunnableContractClass::V0(_) => Err(self.handle_error(
                remaining_gas,
                SyscallExecutionError::ForbiddenClassReplacement { class_hash },
            )),
            RunnableContractClass::V1(_) | RunnableContractClass::V1Native(_) => {
                self.state
                    .set_class_hash_at(self.call.storage_address, class_hash)
                    .map_err(|e| self.handle_error(remaining_gas, e.into()))?;

                Ok(())
            }
        }
    }

    fn library_call(
        &mut self,
        class_hash: Felt,
        function_selector: Felt,
        calldata: &[Felt],
        remaining_gas: &mut u128,
    ) -> SyscallResult<Vec<Felt>> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().library_call_gas_cost)?;

        let class_hash = ClassHash(class_hash);

        let wrapper_calldata = Calldata(Arc::new(calldata.to_vec()));

        let entry_point = CallEntryPoint {
            class_hash: Some(class_hash),
            code_address: None,
            entry_point_type: EntryPointType::External,
            entry_point_selector: EntryPointSelector(function_selector),
            calldata: wrapper_calldata,
            // The call context remains the same in a library call.
            storage_address: self.call.storage_address,
            caller_address: self.call.caller_address,
            call_type: CallType::Delegate,
            initial_gas: u64::try_from(*remaining_gas)
                .expect("Failed to convert gas (u128 -> u64)"),
        };

        Ok(self.execute_inner_call(entry_point, remaining_gas)?.0)
    }

    fn call_contract(
        &mut self,
        address: Felt,
        entry_point_selector: Felt,
        calldata: &[Felt],
        remaining_gas: &mut u128,
    ) -> SyscallResult<Vec<Felt>> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().call_contract_gas_cost)?;

        let contract_address = ContractAddress::try_from(address)
            .map_err(|error| self.handle_error(remaining_gas, error.into()))?;
        if self.context.execution_mode == ExecutionMode::Validate
            && self.call.storage_address != contract_address
        {
            let err = SyscallExecutionError::InvalidSyscallInExecutionMode {
                syscall_name: "call_contract".to_string(),
                execution_mode: self.context.execution_mode,
            };
            return Err(self.handle_error(remaining_gas, err));
        }

        let wrapper_calldata = Calldata(Arc::new(calldata.to_vec()));

        let entry_point = CallEntryPoint {
            class_hash: None,
            code_address: Some(contract_address),
            entry_point_type: EntryPointType::External,
            entry_point_selector: EntryPointSelector(entry_point_selector),
            calldata: wrapper_calldata,
            storage_address: contract_address,
            caller_address: self.call.caller_address,
            call_type: CallType::Call,
            initial_gas: u64::try_from(*remaining_gas)
                .expect("Failed to convert gas from u128 to u64."),
        };

        Ok(self.execute_inner_call(entry_point, remaining_gas)?.0)
    }

    fn storage_read(
        &mut self,
        address_domain: u32,
        address: Felt,
        remaining_gas: &mut u128,
    ) -> SyscallResult<Felt> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().storage_read_gas_cost)?;

        if address_domain != 0 {
            let address_domain = Felt::from(address_domain);
            let error = SyscallExecutionError::InvalidAddressDomain { address_domain };
            return Err(self.handle_error(remaining_gas, error));
        }

        let key = StorageKey::try_from(address)
            .map_err(|e| self.handle_error(remaining_gas, e.into()))?;

        let read_result = self.state.get_storage_at(self.call.storage_address, key);
        let value = read_result.map_err(|e| self.handle_error(remaining_gas, e.into()))?;

        self.accessed_keys.insert(key);
        self.read_values.push(value);

        Ok(value)
    }

    fn storage_write(
        &mut self,
        address_domain: u32,
        address: Felt,
        value: Felt,
        remaining_gas: &mut u128,
    ) -> SyscallResult<()> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().storage_write_gas_cost)?;

        if address_domain != 0 {
            let address_domain = Felt::from(address_domain);
            let error = SyscallExecutionError::InvalidAddressDomain { address_domain };
            return Err(self.handle_error(remaining_gas, error));
        }

        let key = StorageKey::try_from(address)
            .map_err(|e| self.handle_error(remaining_gas, e.into()))?;
        self.accessed_keys.insert(key);

        let write_result = self.state.set_storage_at(self.call.storage_address, key, value);
        write_result.map_err(|e| self.handle_error(remaining_gas, e.into()))?;

        Ok(())
    }

    fn emit_event(
        &mut self,
        keys: &[Felt],
        data: &[Felt],
        remaining_gas: &mut u128,
    ) -> SyscallResult<()> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().emit_event_gas_cost)?;

        let order = self.context.n_emitted_events;
        let event = EventContent {
            keys: keys.iter().copied().map(EventKey).collect(),
            data: EventData(data.to_vec()),
        };

        exceeds_event_size_limit(
            self.context.versioned_constants(),
            self.context.n_emitted_events + 1,
            &event,
        )
        .map_err(|e| self.handle_error(remaining_gas, e.into()))?;

        self.events.push(OrderedEvent { order, event });
        self.context.n_emitted_events += 1;

        Ok(())
    }

    fn send_message_to_l1(
        &mut self,
        to_address: Felt,
        payload: &[Felt],
        remaining_gas: &mut u128,
    ) -> SyscallResult<()> {
        self.pre_execute_syscall(
            remaining_gas,
            self.context.gas_costs().send_message_to_l1_gas_cost,
        )?;

        let order = self.context.n_sent_messages_to_l1;
        let to_address = EthAddress::try_from(to_address)
            .map_err(|e| self.handle_error(remaining_gas, e.into()))?;
        self.l2_to_l1_messages.push(OrderedL2ToL1Message {
            order,
            message: MessageToL1 { to_address, payload: L2ToL1Payload(payload.to_vec()) },
        });

        self.context.n_sent_messages_to_l1 += 1;

        Ok(())
    }

    fn keccak(&mut self, input: &[u64], remaining_gas: &mut u128) -> SyscallResult<U256> {
        self.pre_execute_syscall(remaining_gas, self.context.gas_costs().keccak_gas_cost)?;

        const KECCAK_FULL_RATE_IN_WORDS: usize = 17;

        let input_length = input.len();
        let (n_rounds, remainder) = num_integer::div_rem(input_length, KECCAK_FULL_RATE_IN_WORDS);

        if remainder != 0 {
            return Err(self.handle_error(
                remaining_gas,
                SyscallExecutionError::SyscallError {
                    error_data: vec![Felt::from_hex(INVALID_INPUT_LENGTH_ERROR).unwrap()],
                },
            ));
        }

        // TODO(Ori, 1/2/2024): Write an indicative expect message explaining why the conversion
        // works.
        let n_rounds_as_u128 = u128::try_from(n_rounds).expect("Failed to convert usize to u128.");
        let gas_cost =
            n_rounds_as_u128 * u128::from(self.context.gas_costs().keccak_round_cost_gas_cost);

        if gas_cost > *remaining_gas {
            return Err(self.handle_error(
                remaining_gas,
                SyscallExecutionError::SyscallError {
                    error_data: vec![Felt::from_hex(OUT_OF_GAS_ERROR).unwrap()],
                },
            ));
        }
        *remaining_gas -= gas_cost;

        let mut state = [0u64; 25];
        for chunk in input.chunks(KECCAK_FULL_RATE_IN_WORDS) {
            for (i, val) in chunk.iter().enumerate() {
                state[i] ^= val;
            }
            keccak::f1600(&mut state)
        }

        Ok(U256 {
            hi: u128::from(state[2]) | (u128::from(state[3]) << 64),
            lo: u128::from(state[0]) | (u128::from(state[1]) << 64),
        })
    }

    fn secp256k1_new(
        &mut self,
        _x: U256,
        _y: U256,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Option<Secp256k1Point>> {
        todo!("Implement secp256k1_new syscall.");
    }

    fn secp256k1_add(
        &mut self,
        _p0: Secp256k1Point,
        _p1: Secp256k1Point,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Secp256k1Point> {
        todo!("Implement secp256k1_add syscall.");
    }

    fn secp256k1_mul(
        &mut self,
        _p: Secp256k1Point,
        _m: U256,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Secp256k1Point> {
        todo!("Implement secp256k1_mul syscall.");
    }

    fn secp256k1_get_point_from_x(
        &mut self,
        _x: U256,
        _y_parity: bool,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Option<Secp256k1Point>> {
        todo!("Implement secp256k1_get_point_from_x syscall.");
    }

    fn secp256k1_get_xy(
        &mut self,
        _p: Secp256k1Point,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<(U256, U256)> {
        todo!("Implement secp256k1_get_xy syscall.");
    }

    fn secp256r1_new(
        &mut self,
        _x: U256,
        _y: U256,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Option<Secp256r1Point>> {
        todo!("Implement secp256r1_new syscall.");
    }

    fn secp256r1_add(
        &mut self,
        _p0: Secp256r1Point,
        _p1: Secp256r1Point,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Secp256r1Point> {
        todo!("Implement secp256r1_add syscall.");
    }

    fn secp256r1_mul(
        &mut self,
        _p: Secp256r1Point,
        _m: U256,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Secp256r1Point> {
        todo!("Implement secp256r1_mul syscall.");
    }

    fn secp256r1_get_point_from_x(
        &mut self,
        _x: U256,
        _y_parity: bool,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<Option<Secp256r1Point>> {
        todo!("Implement secp256r1_get_point_from_x syscall.");
    }

    fn secp256r1_get_xy(
        &mut self,
        _p: Secp256r1Point,
        _remaining_gas: &mut u128,
    ) -> SyscallResult<(U256, U256)> {
        todo!("Implement secp256r1_get_xy syscall.");
    }

    fn sha256_process_block(
        &mut self,
        prev_state: &mut [u32; 8],
        current_block: &[u32; 16],
        remaining_gas: &mut u128,
    ) -> SyscallResult<()> {
        self.pre_execute_syscall(
            remaining_gas,
            self.context.gas_costs().sha256_process_block_gas_cost,
        )?;

        let data_as_bytes = sha2::digest::generic_array::GenericArray::from_exact_iter(
            current_block.iter().flat_map(|x| x.to_be_bytes()),
        )
        .expect(
            "u32.to_be_bytes() returns 4 bytes, and data.len() == 16. So data contains 64 bytes.",
        );

        sha2::compress256(prev_state, &[data_as_bytes]);

        Ok(())
    }
}
