use std::{future::Future, pin::Pin};

use trellis_rs::jobs::keys::JobKeyState;

/// One retained Auth-managed Jobs queue binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobResourceBinding {
    /// Qualified service participant identity.
    pub service: String,
    /// Service-local Job queue name.
    pub job_type: String,
    /// Physical work stream.
    pub stream: String,
    /// Physical durable work consumer.
    pub consumer: String,
}

/// Authoritative keyed coordinator state and the Jobs stream horizon captured after it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobKeySnapshot {
    /// Current distributed coordinator state.
    pub state: JobKeyState,
    /// Lifecycle stream sequence observed after reading that state.
    pub jobs_horizon: u64,
}

/// Auth resource evidence and coordinator lookup used by Jobs advisory and query paths.
pub trait JobResourceResolver: Send + Sync {
    /// Resolve one exact physical Jobs consumer, including retained bindings.
    fn by_consumer<'a>(
        &'a self,
        stream: &'a str,
        consumer: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobResourceBinding>, String>> + Send + 'a>>;

    /// Read one current managed keyed coordinator entry and capture its Jobs stream horizon.
    fn key_snapshot<'a>(
        &'a self,
        service: &'a str,
        job_type: &'a str,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobKeySnapshot>, String>> + Send + 'a>>;
}
