//! Jobs resource resolution without access to another role's database.

use std::{future::Future, pin::Pin};

use futures_util::StreamExt;
use trellis_jobs_runtime::{JobKeySnapshot, JobResourceBinding, JobResourceResolver};
use trellis_runtime_apis::types::{
    AuthResourceBindingsQueryRequest, AuthResourceEvidenceKind, AuthResourceEvidenceState,
};

pub(super) struct AuthJobResourceResolver {
    pub(super) evidence: crate::resource_evidence::ResourceEvidenceReader,
    pub(super) nats: async_nats::Client,
    pub(super) jobs_stream: String,
}

impl JobResourceResolver for AuthJobResourceResolver {
    fn by_consumer<'a>(
        &'a self,
        stream: &'a str,
        consumer: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobResourceBinding>, String>> + Send + 'a>> {
        Box::pin(async move {
            let rows = self
                .evidence
                .query(AuthResourceBindingsQueryRequest {
                    instance_id: None,
                    kind: AuthResourceEvidenceKind::JobQueue,
                    owner_id: None,
                    participant_id: None,
                    local_name: None,
                    resource_id: None,
                    page: None,
                    extra: Default::default(),
                })
                .await?;
            Ok(rows.into_iter().find_map(|row| {
                if !matches!(
                    row.state,
                    AuthResourceEvidenceState::Available | AuthResourceEvidenceState::Stale
                ) {
                    return None;
                }
                let job = row.job?;
                (job.work_stream == stream && job.consumer == consumer).then_some(
                    JobResourceBinding {
                        service: row.participant_id,
                        job_type: row.local_name,
                        stream: job.work_stream,
                        consumer: job.consumer,
                    },
                )
            }))
        })
    }

    fn by_publish_prefix<'a>(
        &'a self,
        publish_prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobResourceBinding>, String>> + Send + 'a>> {
        Box::pin(async move {
            let rows = self
                .evidence
                .query(AuthResourceBindingsQueryRequest {
                    instance_id: None,
                    kind: AuthResourceEvidenceKind::JobQueue,
                    owner_id: None,
                    participant_id: None,
                    local_name: None,
                    resource_id: None,
                    page: None,
                    extra: Default::default(),
                })
                .await?;
            Ok(rows.into_iter().find_map(|row| {
                if !matches!(
                    row.state,
                    AuthResourceEvidenceState::Available | AuthResourceEvidenceState::Stale
                ) {
                    return None;
                }
                let job = row.job?;
                (job.publish_prefix == publish_prefix).then_some(JobResourceBinding {
                    service: row.participant_id,
                    job_type: row.local_name,
                    stream: job.work_stream,
                    consumer: job.consumer,
                })
            }))
        })
    }

    fn key_snapshot<'a>(
        &'a self,
        service: &'a str,
        job_type: &'a str,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobKeySnapshot>, String>> + Send + 'a>> {
        Box::pin(async move {
            let rows = self
                .evidence
                .query(AuthResourceBindingsQueryRequest {
                    instance_id: None,
                    kind: AuthResourceEvidenceKind::JobQueue,
                    owner_id: None,
                    participant_id: Some(service.into()),
                    local_name: Some(job_type.into()),
                    resource_id: None,
                    page: None,
                    extra: Default::default(),
                })
                .await?;
            let mut current = rows
                .into_iter()
                .filter(|row| row.resource_bindings_digest.is_some());
            let Some(binding) = current.next().and_then(|row| row.job) else {
                return Ok(None);
            };
            if current.next().is_some() {
                return Err("Job queue has multiple current deployment bindings".into());
            }
            let jetstream = async_nats::jetstream::new(self.nats.clone());
            let store = jetstream
                .get_key_value(format!("JOBS_KEYS_{}", binding.namespace))
                .await
                .map_err(|error| error.to_string())?;
            let mut keys = store.keys().await.map_err(|error| error.to_string())?;
            while let Some(candidate) = keys.next().await {
                let Some(entry) = store
                    .entry(candidate.map_err(|error| error.to_string())?)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    continue;
                };
                let state: trellis_rs::jobs::keys::JobKeyState =
                    serde_json::from_slice(&entry.value).map_err(|error| error.to_string())?;
                if state.key == key && state.job_type == job_type {
                    let mut stream = jetstream
                        .get_stream(&self.jobs_stream)
                        .await
                        .map_err(|error| error.to_string())?;
                    let jobs_horizon = stream
                        .info()
                        .await
                        .map_err(|error| error.to_string())?
                        .state
                        .last_sequence;
                    return Ok(Some(JobKeySnapshot {
                        state,
                        jobs_horizon,
                    }));
                }
            }
            Ok(None)
        })
    }
}
