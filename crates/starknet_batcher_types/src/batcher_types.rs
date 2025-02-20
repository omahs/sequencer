use std::fmt::Debug;

use chrono::prelude::*;
use serde::{Deserialize, Serialize};
use starknet_api::block::{BlockHashAndNumber, BlockNumber};
use starknet_api::core::StateDiffCommitment;
use starknet_api::executable_transaction::Transaction;

use crate::errors::BatcherError;

// TODO (Matan) decide on the id structure
#[derive(
    Copy,
    Clone,
    Debug,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    derive_more::Display,
    Hash,
)]
pub struct ProposalId(pub u64);

#[derive(Clone, Debug, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProposalCommitment {
    pub state_diff_commitment: StateDiffCommitment,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildProposalInput {
    pub proposal_id: ProposalId,
    pub deadline: chrono::DateTime<Utc>,
    pub retrospective_block_hash: Option<BlockHashAndNumber>,
    // TODO: Should we get the gas price here?
    // TODO: add proposer address.
    // TODO: add whether the kzg mechanism is used for DA.
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetProposalContentInput {
    // TBD: We don't really need the proposal_id because there is only one proposal at a time.
    pub proposal_id: ProposalId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetProposalContentResponse {
    pub content: GetProposalContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GetProposalContent {
    Txs(Vec<Transaction>),
    Finished(ProposalCommitment),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// TODO(Dan): Consider unifying with BuildProposalInput as they have the same fields.
pub struct ValidateProposalInput {
    pub proposal_id: ProposalId,
    pub deadline: chrono::DateTime<Utc>,
    pub retrospective_block_hash: Option<BlockHashAndNumber>,
}

impl BuildProposalInput {
    pub fn deadline_as_instant(&self) -> Result<std::time::Instant, chrono::OutOfRangeError> {
        let time_to_deadline = self.deadline - chrono::Utc::now();
        let as_duration = time_to_deadline.to_std()?;
        Ok(std::time::Instant::now() + as_duration)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendProposalContentInput {
    pub proposal_id: ProposalId,
    pub content: SendProposalContent,
}

/// The content of the stream that the consensus sends to the batcher.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SendProposalContent {
    Txs(Vec<Transaction>),
    Finish,
    Abort,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendProposalContentResponse {
    pub response: ProposalStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProposalStatus {
    Processing,
    // Only sent in response to `Finish`.
    Finished(ProposalCommitment),
    // May be caused due to handling of a previous item of the new proposal.
    // In this case, the propsal is aborted and no additional content will be processed.
    InvalidProposal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartHeightInput {
    pub height: BlockNumber,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionReachedInput {
    pub proposal_id: ProposalId,
}

pub type BatcherResult<T> = Result<T, BatcherError>;
