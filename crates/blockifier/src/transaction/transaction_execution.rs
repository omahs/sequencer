use std::sync::Arc;

use cairo_vm::vm::runners::cairo_runner::ExecutionResources;
use starknet_api::contract_class::ClassInfo;
use starknet_api::core::{calculate_contract_address, ContractAddress, Nonce};
use starknet_api::executable_transaction::{
    AccountTransaction as ApiExecutableTransaction,
    DeclareTransaction,
    DeployAccountTransaction,
    InvokeTransaction,
    L1HandlerTransaction,
};
use starknet_api::transaction::fields::Fee;
use starknet_api::transaction::{Transaction as StarknetApiTransaction, TransactionHash};

use crate::bouncer::verify_tx_weights_within_max_capacity;
use crate::context::BlockContext;
use crate::execution::call_info::CallInfo;
use crate::execution::entry_point::EntryPointExecutionContext;
use crate::fee::receipt::TransactionReceipt;
use crate::state::cached_state::TransactionalState;
use crate::state::state_api::UpdatableState;
use crate::transaction::account_transaction::AccountTransaction;
use crate::transaction::errors::TransactionFeeError;
use crate::transaction::objects::{
    TransactionExecutionInfo,
    TransactionExecutionResult,
    TransactionInfo,
    TransactionInfoCreator,
};
use crate::transaction::transactions::{Executable, ExecutableTransaction, ExecutionFlags};

// TODO: Move into transaction.rs, makes more sense to be defined there.
#[derive(Clone, Debug, derive_more::From)]
pub enum Transaction {
    Account(AccountTransaction),
    L1Handler(L1HandlerTransaction),
}

impl From<starknet_api::executable_transaction::Transaction> for Transaction {
    fn from(value: starknet_api::executable_transaction::Transaction) -> Self {
        match value {
            starknet_api::executable_transaction::Transaction::Account(tx) => {
                Transaction::Account(tx.into())
            }
            starknet_api::executable_transaction::Transaction::L1Handler(tx) => {
                Transaction::L1Handler(tx)
            }
        }
    }
}

impl Transaction {
    pub fn nonce(&self) -> Nonce {
        match self {
            Self::Account(tx) => tx.nonce(),
            Self::L1Handler(tx) => tx.tx.nonce,
        }
    }

    pub fn sender_address(&self) -> ContractAddress {
        match self {
            Self::Account(tx) => tx.sender_address(),
            Self::L1Handler(tx) => tx.tx.contract_address,
        }
    }

    pub fn tx_hash(tx: &Transaction) -> TransactionHash {
        match tx {
            Transaction::Account(tx) => tx.tx_hash(),
            Transaction::L1Handler(tx) => tx.tx_hash,
        }
    }

    pub fn from_api(
        tx: StarknetApiTransaction,
        tx_hash: TransactionHash,
        class_info: Option<ClassInfo>,
        paid_fee_on_l1: Option<Fee>,
        deployed_contract_address: Option<ContractAddress>,
        only_query: bool,
    ) -> TransactionExecutionResult<Self> {
        let executable_tx = match tx {
            StarknetApiTransaction::L1Handler(l1_handler) => {
                return Ok(Self::L1Handler(L1HandlerTransaction {
                    tx: l1_handler,
                    tx_hash,
                    paid_fee_on_l1: paid_fee_on_l1
                        .expect("L1Handler should be created with the fee paid on L1"),
                }));
            }
            StarknetApiTransaction::Declare(declare) => {
                let non_optional_class_info =
                    class_info.expect("Declare should be created with a ClassInfo.");

                ApiExecutableTransaction::Declare(DeclareTransaction {
                    tx: declare,
                    tx_hash,
                    class_info: non_optional_class_info,
                })
            }
            StarknetApiTransaction::DeployAccount(deploy_account) => {
                let contract_address = match deployed_contract_address {
                    Some(address) => address,
                    None => calculate_contract_address(
                        deploy_account.contract_address_salt(),
                        deploy_account.class_hash(),
                        &deploy_account.constructor_calldata(),
                        ContractAddress::default(),
                    )?,
                };
                ApiExecutableTransaction::DeployAccount(DeployAccountTransaction {
                    tx: deploy_account,
                    tx_hash,
                    contract_address,
                })
            }
            StarknetApiTransaction::Invoke(invoke) => {
                ApiExecutableTransaction::Invoke(InvokeTransaction { tx: invoke, tx_hash })
            }
            _ => unimplemented!(),
        };
        let account_tx = match only_query {
            true => AccountTransaction::new_for_query(executable_tx),
            false => AccountTransaction::new(executable_tx),
        };
        Ok(account_tx.into())
    }
}

impl TransactionInfoCreator for Transaction {
    fn create_tx_info(&self) -> TransactionInfo {
        match self {
            Self::Account(account_tx) => account_tx.create_tx_info(),
            Self::L1Handler(l1_handler_tx) => l1_handler_tx.create_tx_info(),
        }
    }
}

impl<U: UpdatableState> ExecutableTransaction<U> for L1HandlerTransaction {
    fn execute_raw(
        &self,
        state: &mut TransactionalState<'_, U>,
        block_context: &BlockContext,
        _execution_flags: ExecutionFlags,
    ) -> TransactionExecutionResult<TransactionExecutionInfo> {
        let tx_context = Arc::new(block_context.to_tx_context(self));
        let limit_steps_by_resources = false;
        let mut execution_resources = ExecutionResources::default();
        let mut context =
            EntryPointExecutionContext::new_invoke(tx_context.clone(), limit_steps_by_resources);
        let mut remaining_gas = tx_context.initial_sierra_gas();
        let execute_call_info =
            self.run_execute(state, &mut execution_resources, &mut context, &mut remaining_gas)?;
        let l1_handler_payload_size = self.payload_size();

        let TransactionReceipt {
            fee: actual_fee,
            da_gas,
            resources: actual_resources,
            gas: total_gas,
        } = TransactionReceipt::from_l1_handler(
            &tx_context,
            l1_handler_payload_size,
            CallInfo::summarize_many(execute_call_info.iter()),
            &state.get_actual_state_changes()?,
            &execution_resources,
        );

        let paid_fee = self.paid_fee_on_l1;
        // For now, assert only that any amount of fee was paid.
        // The error message still indicates the required fee.
        if paid_fee == Fee(0) {
            return Err(TransactionFeeError::InsufficientFee { paid_fee, actual_fee })?;
        }

        Ok(TransactionExecutionInfo {
            validate_call_info: None,
            execute_call_info,
            fee_transfer_call_info: None,
            receipt: TransactionReceipt {
                fee: Fee::default(),
                da_gas,
                resources: actual_resources,
                gas: total_gas,
            },
            revert_error: None,
        })
    }
}

impl<U: UpdatableState> ExecutableTransaction<U> for Transaction {
    fn execute_raw(
        &self,
        state: &mut TransactionalState<'_, U>,
        block_context: &BlockContext,
        execution_flags: ExecutionFlags,
    ) -> TransactionExecutionResult<TransactionExecutionInfo> {
        // TODO(Yoni, 1/8/2024): consider unimplementing the ExecutableTransaction trait for inner
        // types, since now running Transaction::execute_raw is not identical to
        // AccountTransaction::execute_raw.
        let concurrency_mode = execution_flags.concurrency_mode;
        let tx_execution_info = match self {
            Self::Account(account_tx) => {
                account_tx.execute_raw(state, block_context, execution_flags)?
            }
            Self::L1Handler(tx) => tx.execute_raw(state, block_context, execution_flags)?,
        };

        // Check if the transaction is too large to fit any block.
        // TODO(Yoni, 1/8/2024): consider caching these two.
        let tx_execution_summary = tx_execution_info.summarize();
        let mut tx_state_changes_keys = state.get_actual_state_changes()?.into_keys();
        tx_state_changes_keys.update_sequencer_key_in_storage(
            &block_context.to_tx_context(self),
            &tx_execution_info,
            concurrency_mode,
        );
        verify_tx_weights_within_max_capacity(
            state,
            &tx_execution_summary,
            &tx_execution_info.receipt.resources,
            &tx_state_changes_keys,
            &block_context.bouncer_config,
        )?;

        Ok(tx_execution_info)
    }
}
