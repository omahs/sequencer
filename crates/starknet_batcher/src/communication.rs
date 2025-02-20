use async_trait::async_trait;
use starknet_batcher_types::communication::{BatcherRequest, BatcherResponse};
use starknet_sequencer_infra::component_definitions::ComponentRequestHandler;
use starknet_sequencer_infra::component_server::{LocalComponentServer, RemoteComponentServer};

use crate::batcher::Batcher;

pub type LocalBatcherServer = LocalComponentServer<Batcher, BatcherRequest, BatcherResponse>;
pub type RemoteBatcherServer = RemoteComponentServer<BatcherRequest, BatcherResponse>;

#[async_trait]
impl ComponentRequestHandler<BatcherRequest, BatcherResponse> for Batcher {
    async fn handle_request(&mut self, request: BatcherRequest) -> BatcherResponse {
        match request {
            BatcherRequest::BuildProposal(input) => {
                BatcherResponse::BuildProposal(self.build_proposal(input).await)
            }
            BatcherRequest::GetProposalContent(input) => {
                BatcherResponse::GetProposalContent(self.get_proposal_content(input).await)
            }
            BatcherRequest::StartHeight(input) => {
                BatcherResponse::StartHeight(self.start_height(input).await)
            }
            BatcherRequest::DecisionReached(input) => {
                BatcherResponse::DecisionReached(self.decision_reached(input).await)
            }
            BatcherRequest::ValidateProposal(input) => {
                BatcherResponse::ValidateProposal(self.validate_proposal(input).await)
            }
            BatcherRequest::SendProposalContent(input) => {
                BatcherResponse::SendProposalContent(self.send_proposal_content(input).await)
            }
        }
    }
}
