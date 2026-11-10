//! Service and device health enrichment for automatic heartbeats.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use serde_json::{Map, Value};

type InfoFn = Arc<dyn Fn() -> BoxFuture<'static, Option<ServiceHealthInfo>> + Send + Sync>;
type CheckFn = Arc<dyn Fn() -> BoxFuture<'static, ServiceHealthCheck> + Send + Sync>;

/// Build and deployment details a participant reports with each heartbeat.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceHealthInfo {
    /// The participant's own version, such as a build or release identifier.
    pub version: Option<String>,
    /// Further JSON details shown with the instance.
    pub info: Option<Map<String, Value>>,
}

/// Status of one named health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthCheckStatus {
    /// The check passed.
    Ok,
    /// The check failed.
    Failed,
}

/// Result of one named health check.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceHealthCheck {
    /// Whether the check passed.
    pub status: HealthCheckStatus,
    /// Short human-readable summary.
    pub summary: Option<String>,
    /// Structured details about the check.
    pub info: Option<Map<String, Value>>,
    /// Error message when the check failed.
    pub error: Option<String>,
}

impl ServiceHealthCheck {
    /// A passing check with no details.
    pub fn ok() -> Self {
        Self {
            status: HealthCheckStatus::Ok,
            summary: None,
            info: None,
            error: None,
        }
    }

    /// A failing check with `error` as its message.
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: HealthCheckStatus::Failed,
            summary: None,
            info: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Default)]
struct HealthState {
    info: Option<InfoFn>,
    checks: BTreeMap<String, (u64, CheckFn)>,
    next_registration: u64,
}

/// Health details and checks added to a connected participant's heartbeats.
///
/// Every heartbeat calls the info callback and runs each check, so the samples
/// report current state. Clones share the same details and checks.
#[derive(Clone, Default)]
pub struct ServiceHealth {
    state: Arc<Mutex<HealthState>>,
}

impl std::fmt::Debug for ServiceHealth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().expect("health state");
        formatter
            .debug_struct("ServiceHealth")
            .field("checks", &state.checks.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Registration of one health check; [`HealthCheckRegistration::remove`]
/// removes it. Dropping the registration keeps the check.
#[derive(Debug)]
pub struct HealthCheckRegistration {
    health: ServiceHealth,
    name: String,
    id: u64,
}

impl HealthCheckRegistration {
    /// Stop running this check. A later check added under the same name stays.
    pub fn remove(self) {
        let mut state = self.health.state.lock().expect("health state");
        if state
            .checks
            .get(&self.name)
            .is_some_and(|(id, _)| *id == self.id)
        {
            state.checks.remove(&self.name);
        }
    }
}

impl ServiceHealth {
    /// Report fixed details with every heartbeat.
    pub fn set_info(&self, info: ServiceHealthInfo) {
        self.set_info_with(move || std::future::ready(Some(info.clone())));
    }

    /// Report details computed when each heartbeat is published; `None` sends none.
    pub fn set_info_with<F, Fut>(&self, info: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<ServiceHealthInfo>> + Send + 'static,
    {
        self.state.lock().expect("health state").info = Some(Arc::new(move || info().boxed()));
    }

    /// Run `check` with every heartbeat under `name`, replacing any check of
    /// that name. A check that panics is reported as failed.
    pub fn add<F, Fut>(&self, name: impl Into<String>, check: F) -> HealthCheckRegistration
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ServiceHealthCheck> + Send + 'static,
    {
        let name = name.into();
        let mut state = self.state.lock().expect("health state");
        state.next_registration += 1;
        let id = state.next_registration;
        state
            .checks
            .insert(name.clone(), (id, Arc::new(move || check().boxed())));
        HealthCheckRegistration {
            health: self.clone(),
            name,
            id,
        }
    }

    pub(crate) async fn snapshot(&self) -> HealthSnapshot {
        let (info, checks) = {
            let state = self.state.lock().expect("health state");
            (
                state.info.clone(),
                state
                    .checks
                    .iter()
                    .map(|(name, (_, check))| (name.clone(), Arc::clone(check)))
                    .collect::<Vec<_>>(),
            )
        };
        let checks =
            futures_util::future::join_all(checks.into_iter().map(|(name, check)| async move {
                let started = Instant::now();
                let result = std::panic::AssertUnwindSafe(check())
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| ServiceHealthCheck::failed("health check panicked"));
                (name, result, started.elapsed().as_secs_f64() * 1000.0)
            }))
            .await;
        let info = match info {
            Some(info) => info().await,
            None => None,
        };
        HealthSnapshot { info, checks }
    }
}

pub(crate) struct HealthSnapshot {
    pub(crate) info: Option<ServiceHealthInfo>,
    /// Name, result and latency in milliseconds of each check.
    pub(crate) checks: Vec<(String, ServiceHealthCheck, f64)>,
}
