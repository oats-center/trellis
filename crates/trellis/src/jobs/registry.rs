//! Worker-heartbeat and cancellation helpers for jobs workers.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::jobs::runtime_worker::JobCancellationToken;
use crate::jobs::subjects::worker_heartbeat_subject;
use crate::jobs::types::WorkerHeartbeat;

/// Errors returned while publishing or maintaining worker heartbeats.
#[derive(Debug, thiserror::Error)]
#[doc = concat!("Public Trellis value set `", stringify!(ServiceRegistryError), "`.")]
pub enum ServiceRegistryError {
    #[error("worker heartbeat task failed: {details}")]
    HeartbeatTask { details: String },
    #[error("failed to encode worker heartbeat for subject '{subject}': {details}")]
    EncodeWorkerHeartbeat { subject: String, details: String },
    #[error("failed to publish worker heartbeat on subject '{subject}': {details}")]
    PublishWorkerHeartbeat { subject: String, details: String },
}

/// Handle for a background worker heartbeat loop.
#[doc = concat!("Public Trellis data type `", stringify!(WorkerHeartbeatHandle), "`.")]
pub struct WorkerHeartbeatHandle {
    task: tokio::task::JoinHandle<Result<(), ServiceRegistryError>>,
}

/// Identity and scheduling options for one worker heartbeat loop.
#[derive(Debug, Clone)]
pub struct WorkerHeartbeatOptions {
    /// Service name recorded in the heartbeat payload.
    pub service: String,
    /// Service namespace used in the heartbeat subject.
    pub subject_service: String,
    /// Queue type processed by the worker.
    pub job_type: String,
    /// Worker-host instance identifier.
    pub instance_id: String,
    /// Queue concurrency advertised by the worker host.
    pub concurrency: Option<u32>,
    /// Optional worker version advertised in the heartbeat.
    pub version: Option<String>,
    /// Delay between heartbeat publications.
    pub interval: Duration,
}

impl WorkerHeartbeatHandle {
    /// Stop the heartbeat task and swallow expected cancellation shutdown.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(stop), "`.")]
    pub async fn stop(self) -> Result<(), ServiceRegistryError> {
        self.task.abort();
        match self.task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(ServiceRegistryError::HeartbeatTask {
                details: error.to_string(),
            }),
        }
    }
}

#[derive(Clone, Default)]
#[doc = concat!("Public Trellis data type `", stringify!(ActiveJobCancellationRegistry), "`.")]
pub struct ActiveJobCancellationRegistry {
    inner: Arc<Mutex<ActiveJobCancellationRegistryInner>>,
}

#[derive(Default)]
struct ActiveJobCancellationRegistryInner {
    tokens: HashMap<String, Vec<JobCancellationToken>>,
    pending: HashSet<String>,
}

#[doc = concat!("Public Trellis data type `", stringify!(ActiveJobCancellationGuard), "`.")]
pub struct ActiveJobCancellationGuard {
    key: String,
    token: JobCancellationToken,
    registry: ActiveJobCancellationRegistry,
}

impl ActiveJobCancellationRegistry {
    #[doc = concat!("Trellis API operation `", stringify!(new), "`.")]
    pub fn new() -> Self {
        Self::default()
    }

    #[doc = concat!("Trellis API operation `", stringify!(register), "`.")]
    pub fn register(
        &self,
        key: impl Into<String>,
        token: JobCancellationToken,
    ) -> ActiveJobCancellationGuard {
        let key = key.into();
        let mut inner = self.inner.lock().expect("lock cancellation registry");
        inner
            .tokens
            .entry(key.clone())
            .or_default()
            .push(token.clone());
        if inner.pending.remove(&key) {
            token.cancel();
        }
        ActiveJobCancellationGuard {
            key,
            token,
            registry: self.clone(),
        }
    }

    #[doc = concat!("Trellis API operation `", stringify!(cancel), "`.")]
    pub fn cancel(&self, key: &str) -> bool {
        let mut found = false;
        let mut inner = self.inner.lock().expect("lock cancellation registry");
        if let Some(tokens) = inner.tokens.get(key) {
            for token in tokens {
                token.cancel();
                found = true;
            }
        }
        if !found {
            inner.pending.insert(key.to_string());
        }
        found
    }

    /// Forget a pending cancel for work that will never register a live token.
    pub fn clear_pending(&self, key: &str) {
        let mut inner = self.inner.lock().expect("lock cancellation registry");
        inner.pending.remove(key);
    }

    fn unregister(&self, key: &str, token: &JobCancellationToken) {
        let mut inner = self.inner.lock().expect("lock cancellation registry");
        if let Some(tokens) = inner.tokens.get_mut(key) {
            tokens.retain(|existing| !existing.is_same_token(token));
            if tokens.is_empty() {
                inner.tokens.remove(key);
            }
        }
    }
}

impl Drop for ActiveJobCancellationGuard {
    fn drop(&mut self) {
        self.registry.unregister(&self.key, &self.token);
    }
}

/// Build a fresh worker heartbeat payload.
#[doc = concat!("Trellis API operation `", stringify!(new_worker_heartbeat), "`.")]
pub fn new_worker_heartbeat(
    service: &str,
    job_type: &str,
    instance_id: &str,
    concurrency: Option<u32>,
    version: Option<String>,
    now_iso: String,
) -> WorkerHeartbeat {
    WorkerHeartbeat {
        service: service.to_string(),
        job_type: job_type.to_string(),
        instance_id: instance_id.to_string(),
        concurrency,
        version,
        timestamp: now_iso,
    }
}

/// Publish one worker heartbeat immediately.
#[doc = concat!("Asynchronous Trellis API operation `", stringify!(publish_worker_heartbeat), "`.")]
pub async fn publish_worker_heartbeat(
    nats: async_nats::Client,
    heartbeat: &WorkerHeartbeat,
) -> Result<(), ServiceRegistryError> {
    publish_worker_heartbeat_for_subject(nats, &heartbeat.service, heartbeat).await
}

async fn publish_worker_heartbeat_for_subject(
    nats: async_nats::Client,
    subject_service: &str,
    heartbeat: &WorkerHeartbeat,
) -> Result<(), ServiceRegistryError> {
    let subject =
        worker_heartbeat_subject(subject_service, &heartbeat.job_type, &heartbeat.instance_id);
    let payload = serde_json::to_vec(heartbeat).map_err(|error| {
        ServiceRegistryError::EncodeWorkerHeartbeat {
            subject: subject.clone(),
            details: error.to_string(),
        }
    })?;
    nats.publish(subject.clone(), payload.into())
        .await
        .map_err(|error| ServiceRegistryError::PublishWorkerHeartbeat {
            subject,
            details: error.to_string(),
        })?;
    Ok(())
}

/// Start a background heartbeat loop for one worker-host queue type.
#[doc = concat!("Asynchronous Trellis API operation `", stringify!(start_worker_heartbeat_loop), "`.")]
pub async fn start_worker_heartbeat_loop(
    nats: async_nats::Client,
    options: WorkerHeartbeatOptions,
) -> Result<WorkerHeartbeatHandle, ServiceRegistryError> {
    let WorkerHeartbeatOptions {
        service,
        subject_service,
        job_type,
        instance_id,
        concurrency,
        version,
        interval,
    } = options;
    let publish = move |nats: async_nats::Client, timestamp: String| {
        let heartbeat = new_worker_heartbeat(
            &service,
            &job_type,
            &instance_id,
            concurrency,
            version.clone(),
            timestamp,
        );
        let subject_service = subject_service.clone();
        async move { publish_worker_heartbeat_for_subject(nats, &subject_service, &heartbeat).await }
    };

    publish(nats.clone(), now_timestamp_string()).await?;

    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        loop {
            ticker.tick().await;
            publish(nats.clone(), now_timestamp_string()).await?;
        }
    });

    Ok(WorkerHeartbeatHandle { task })
}

fn now_timestamp_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
