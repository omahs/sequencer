#[cfg(test)]
mod test;

use async_trait::async_trait;
use futures::stream::FuturesUnordered;
use futures::{pin_mut, FutureExt, StreamExt, TryFutureExt};
use papyrus_network::network_manager::{
    BroadcastTopicClient,
    BroadcastTopicClientTrait,
    BroadcastTopicServer,
    NetworkManager,
};
use papyrus_protobuf::mempool::RpcTransactionWrapper;
use starknet_gateway_types::communication::{GatewayClientError, SharedGatewayClient};
use starknet_gateway_types::errors::GatewayError;
use starknet_gateway_types::gateway_types::GatewayInput;
use starknet_sequencer_infra::component_definitions::ComponentStarter;
use starknet_sequencer_infra::component_server::WrapperServer;
use starknet_sequencer_infra::errors::ComponentError;
use tracing::warn;

pub struct MempoolP2pRunner {
    network_manager: Option<NetworkManager>,
    broadcasted_topic_server: BroadcastTopicServer<RpcTransactionWrapper>,
    broadcast_topic_client: BroadcastTopicClient<RpcTransactionWrapper>,
    gateway_client: SharedGatewayClient,
}

impl MempoolP2pRunner {
    pub fn new(
        network_manager: Option<NetworkManager>,
        broadcasted_topic_server: BroadcastTopicServer<RpcTransactionWrapper>,
        broadcast_topic_client: BroadcastTopicClient<RpcTransactionWrapper>,
        gateway_client: SharedGatewayClient,
    ) -> Self {
        Self { network_manager, broadcasted_topic_server, broadcast_topic_client, gateway_client }
    }
}

#[async_trait]
impl ComponentStarter for MempoolP2pRunner {
    async fn start(&mut self) -> Result<(), ComponentError> {
        let network_future = self
            .network_manager
            .take()
            .expect("Network manager not found")
            .run()
            .map_err(|_| ComponentError::InternalComponentError);
        pin_mut!(network_future);
        let mut gateway_futures = FuturesUnordered::new();
        loop {
            tokio::select! {
                // tokio::select! takes ownership of the futures, so we need to wrap with poll_fn
                result = futures::future::poll_fn(|cx| network_future.poll_unpin(cx)) => {
                    result?;
                    panic!("Network stopped unexpectedly");
                }
                Some(result) = gateway_futures.next() => {
                    match result {
                        Ok(_) => {}
                        Err(gateway_client_error) => {
                            if let GatewayClientError::GatewayError(
                                GatewayError::GatewaySpecError{p2p_message_metadata: Some(p2p_message_metadata), ..}
                            ) = gateway_client_error {
                                if let Err(e) = self.broadcast_topic_client.report_peer(p2p_message_metadata.clone()).await {
                                    warn!("Failed to report peer: {:?}", e);
                                }
                            }
                        }
                    }
                }
                Some((message_result, broadcasted_message_metadata)) = self.broadcasted_topic_server.next() => {
                    match message_result {
                        Ok(message) => {
                            gateway_futures.push(self.gateway_client.add_tx(
                                GatewayInput { rpc_tx: message.0, message_metadata: Some(broadcasted_message_metadata.clone()) }
                            ));
                        }
                        Err(e) => {
                            warn!("Received a faulty transaction from network: {:?}. Attempting to report the sending peer", e);
                            if let Err(e) = self.broadcast_topic_client.report_peer(broadcasted_message_metadata).await {
                                warn!("Failed to report peer: {:?}", e);
                            }
                        }
                    }
                }
            }
        }
    }
}

pub type MempoolP2pRunnerServer = WrapperServer<MempoolP2pRunner>;
