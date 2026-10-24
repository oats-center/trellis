//! Auth-owned resource evidence read through ordinary generated RPC clients.

use futures_util::TryStreamExt;
use trellis_rs::__internal::LiveProviderOwner;
use trellis_rs::__internal::RuntimeOwner as _;
use trellis_runtime_apis::{
    apis::trellis_auth_v1,
    types::{AuthResourceBindingsQueryRequest, AuthResourceEvidence},
};

#[derive(Clone)]
pub(crate) struct ResourceEvidenceReader {
    owner: tokio::sync::watch::Receiver<Option<LiveProviderOwner>>,
    stop: crate::shutdown::StopHandle,
}

impl ResourceEvidenceReader {
    pub(crate) fn new(
        owner: tokio::sync::watch::Receiver<Option<LiveProviderOwner>>,
        stop: crate::shutdown::StopHandle,
    ) -> Self {
        Self { owner, stop }
    }

    pub(crate) async fn query(
        &self,
        input: AuthResourceBindingsQueryRequest,
    ) -> Result<Vec<AuthResourceEvidence>, String> {
        let owner = crate::platform::await_live_owner(&mut self.owner.clone(), &self.stop)
            .await
            .ok_or_else(|| "resource evidence provider stopped".to_owned())?;
        let client = trellis_auth_v1::Client::from_generated(owner.runtime_generated_client());
        client
            .resource_bindings_query_items(input)
            .try_collect()
            .await
            .map_err(|error| error.to_string())
    }
}
