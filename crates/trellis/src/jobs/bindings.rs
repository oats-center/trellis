//! Binding helpers for the jobs runtime.

use std::collections::BTreeMap;

use crate::service::ServiceResourceBindings;
use serde::Deserialize;
use serde_json::Value;

/// Resolved service-local jobs binding derived from Trellis resource bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(JobsBinding), "`.")]
pub struct JobsBinding {
    /// Logical registered service name projected in job admin views.
    #[doc = concat!("The `", stringify!(service_name), "` value.")]
    pub service_name: String,
    /// Service namespace used in job subjects.
    #[doc = concat!("The `", stringify!(namespace), "` value.")]
    pub namespace: String,
    /// Queue bindings keyed by logical queue type.
    #[doc = concat!("The `", stringify!(queues), "` value.")]
    pub queues: BTreeMap<String, JobsQueueBinding>,
}

/// Full worker runtime binding including the work stream name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(JobsRuntimeBinding), "`.")]
pub struct JobsRuntimeBinding {
    /// Service-local queue binding data.
    #[doc = concat!("The `", stringify!(jobs), "` value.")]
    pub jobs: JobsBinding,
    /// Bound work stream name used for consumer creation.
    #[doc = concat!("The `", stringify!(work_stream), "` value.")]
    pub work_stream: String,
}

/// Resolved runtime settings for one jobs queue type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(JobsQueueBinding), "`.")]
pub struct JobsQueueBinding {
    /// Logical queue type from the contract binding.
    #[doc = concat!("The `", stringify!(queue_type), "` value.")]
    pub queue_type: String,
    /// Publish prefix for lifecycle events in the jobs stream.
    #[doc = concat!("The `", stringify!(publish_prefix), "` value.")]
    pub publish_prefix: String,
    /// Publish prefix for live-only updates, when declared.
    #[doc = concat!("The `", stringify!(updates_prefix), "` value.")]
    pub updates_prefix: Option<String>,
    /// Work subject consumed by the worker.
    #[doc = concat!("The `", stringify!(work_subject), "` value.")]
    pub work_subject: String,
    /// Durable consumer name for the queue.
    #[doc = concat!("The `", stringify!(consumer_name), "` value.")]
    pub consumer_name: String,
    /// Maximum delivery attempts before advisory-based dead-letter handling.
    #[doc = concat!("The `", stringify!(max_deliver), "` value.")]
    pub max_deliver: u64,
    /// Redelivery backoff schedule in milliseconds.
    #[doc = concat!("The `", stringify!(backoff_ms), "` value.")]
    pub backoff_ms: Vec<u64>,
    /// Ack wait in milliseconds for the durable consumer.
    #[doc = concat!("The `", stringify!(ack_wait_ms), "` value.")]
    pub ack_wait_ms: u64,
    /// Optional business deadline applied to newly created jobs.
    #[doc = concat!("The `", stringify!(default_deadline_ms), "` value.")]
    pub default_deadline_ms: Option<u64>,
    /// Declared update schema name, when live updates are enabled.
    #[doc = concat!("The `", stringify!(update), "` value.")]
    pub update: Option<String>,
    /// Optional normalized keyed concurrency policy.
    #[doc = concat!("The `", stringify!(key_concurrency), "` value.")]
    pub key_concurrency: Option<JobKeyConcurrencyBinding>,
    /// Optional normalized queue-depth policy for keyed queues.
    #[doc = concat!("The `", stringify!(queue), "` value.")]
    pub queue: Option<JobQueueDepthBinding>,
}

/// Normalized keyed concurrency policy for one jobs queue.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobKeyConcurrencyBinding), "`.")]
pub struct JobKeyConcurrencyBinding {
    /// Ordered key template segments.
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: Vec<String>,
    /// Maximum active jobs for one derived key.
    #[doc = concat!("The `", stringify!(max_active), "` value.")]
    pub max_active: u32,
    /// Key lease heartbeat interval in milliseconds.
    #[doc = concat!("The `", stringify!(heartbeat_interval_ms), "` value.")]
    pub heartbeat_interval_ms: u64,
    /// Key lease TTL in milliseconds.
    #[doc = concat!("The `", stringify!(heartbeat_ttl_ms), "` value.")]
    pub heartbeat_ttl_ms: u64,
    /// Behavior for expired active slots.
    #[doc = concat!("The `", stringify!(stale_policy), "` value.")]
    pub stale_policy: JobKeyStalePolicy,
}

/// Stale active-key policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[doc = concat!("Public Trellis value set `", stringify!(JobKeyStalePolicy), "`.")]
pub enum JobKeyStalePolicy {
    /// Mark stale jobs before acquiring their expired slot.
    FailStale,
    /// Block later jobs while an active key slot is stale.
    Block,
}

/// Normalized queue-depth policy for one keyed jobs queue.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobQueueDepthBinding), "`.")]
pub struct JobQueueDepthBinding {
    /// Maximum queued jobs allowed for one derived key.
    #[doc = concat!("The `", stringify!(max_queued_per_key), "` value.")]
    pub max_queued_per_key: u64,
    /// Behavior when the per-key queue is full.
    #[doc = concat!("The `", stringify!(when_full), "` value.")]
    pub when_full: JobQueueWhenFull,
}

/// Queue-full policy for keyed jobs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[doc = concat!("Public Trellis value set `", stringify!(JobQueueWhenFull), "`.")]
pub enum JobQueueWhenFull {
    /// Reject the new job.
    Reject,
    /// Return the existing active or queued job.
    Coalesce,
    /// Replace the oldest queued job for the same key.
    ReplaceOldest,
}

/// Errors returned while decoding jobs bindings from core bootstrap data.
#[derive(Debug, thiserror::Error)]
#[doc = concat!("Public Trellis value set `", stringify!(JobsBindingError), "`.")]
pub enum JobsBindingError {
    #[error("bindings response is missing resources.jobs")]
    MissingJobsResource,
    #[error("bindings response is missing resources.jobs.workStream")]
    MissingWorkStream,
    #[error("invalid jobs queue binding for queue type '{queue_type}': {details}")]
    InvalidQueueBinding { queue_type: String, details: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobsQueueBindingValue {
    publish_prefix: String,
    updates_prefix: Option<String>,
    work_subject: String,
    consumer_name: String,
    max_deliver: u64,
    backoff_ms: Vec<u64>,
    ack_wait_ms: u64,
    default_deadline_ms: Option<u64>,
    update: Option<JobUpdateBindingValue>,
    key_concurrency: Option<JobKeyConcurrencyBindingValue>,
    queue: Option<JobQueueDepthBindingValue>,
}

#[derive(Debug, Deserialize)]
struct JobUpdateBindingValue {
    schema: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobKeyConcurrencyBindingValue {
    key: Vec<String>,
    max_active: u32,
    heartbeat_interval_ms: u64,
    heartbeat_ttl_ms: u64,
    stale_policy: JobKeyStalePolicyValue,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum JobKeyStalePolicyValue {
    FailStale,
    Block,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobQueueDepthBindingValue {
    max_queued_per_key: u64,
    when_full: JobQueueWhenFullValue,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum JobQueueWhenFullValue {
    Reject,
    Coalesce,
    ReplaceOldest,
}

#[derive(Debug)]
struct NormalizedJobsQueueBinding {
    queue_type: String,
    publish_prefix: String,
    updates_prefix: Option<String>,
    work_subject: String,
    consumer_name: String,
    max_deliver: u64,
    backoff_ms: Vec<u64>,
    ack_wait_ms: u64,
    default_deadline_ms: Option<u64>,
    update: Option<String>,
    key_concurrency: Option<JobKeyConcurrencyBinding>,
    queue: Option<JobQueueDepthBinding>,
}

/// Parse a raw jobs binding map into the handwritten runtime binding type.
#[doc = concat!("Trellis API operation `", stringify!(parse_jobs_binding), "`.")]
pub fn parse_jobs_binding(
    service_name: &str,
    namespace: &str,
    queues: &BTreeMap<String, Value>,
) -> Result<JobsBinding, JobsBindingError> {
    let normalized = queues
        .iter()
        .map(|(queue_type, value)| normalize_json_queue_binding(queue_type, value))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(build_jobs_binding(
        service_name.to_string(),
        namespace.to_string(),
        normalized,
    ))
}

impl TryFrom<&ServiceResourceBindings> for JobsRuntimeBinding {
    type Error = JobsBindingError;

    fn try_from(binding: &ServiceResourceBindings) -> Result<Self, Self::Error> {
        let jobs = binding
            .jobs
            .as_ref()
            .ok_or(JobsBindingError::MissingJobsResource)?;
        let normalized = jobs
            .queues
            .iter()
            .map(|(queue_type, queue)| {
                Ok(NormalizedJobsQueueBinding {
                    queue_type: queue.queue_type.clone(),
                    publish_prefix: queue.publish_prefix.clone(),
                    updates_prefix: queue.updates_prefix.clone(),
                    work_subject: queue.work_subject.clone(),
                    consumer_name: queue.consumer_name.clone(),
                    max_deliver: i64_to_u64(queue.max_deliver, queue_type, "maxDeliver")?,
                    backoff_ms: queue
                        .backoff_ms
                        .iter()
                        .copied()
                        .map(|value| i64_to_u64(value, queue_type, "backoffMs"))
                        .collect::<Result<Vec<_>, _>>()?,
                    ack_wait_ms: i64_to_u64(queue.ack_wait_ms, queue_type, "ackWaitMs")?,
                    default_deadline_ms: queue
                        .default_deadline_ms
                        .map(|value| i64_to_u64(value, queue_type, "defaultDeadlineMs"))
                        .transpose()?,
                    update: queue.update.as_ref().map(|update| update.schema.clone()),
                    key_concurrency: queue.key_concurrency.clone(),
                    queue: queue.queue.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let work_stream = jobs
            .work_stream
            .clone()
            .ok_or(JobsBindingError::MissingWorkStream)?;

        Ok(Self {
            jobs: build_jobs_binding(
                jobs.service_name.clone(),
                jobs.namespace.clone(),
                normalized,
            ),
            work_stream,
        })
    }
}

fn build_jobs_binding(
    service_name: String,
    namespace: String,
    queues: Vec<NormalizedJobsQueueBinding>,
) -> JobsBinding {
    JobsBinding {
        service_name,
        namespace,
        queues: queues
            .into_iter()
            .map(|queue| {
                let queue_type = queue.queue_type.clone();
                (queue_type, jobs_queue_binding_from_normalized(queue))
            })
            .collect(),
    }
}

fn jobs_queue_binding_from_normalized(queue: NormalizedJobsQueueBinding) -> JobsQueueBinding {
    JobsQueueBinding {
        queue_type: queue.queue_type,
        publish_prefix: queue.publish_prefix,
        updates_prefix: queue.updates_prefix,
        work_subject: queue.work_subject,
        consumer_name: queue.consumer_name,
        max_deliver: queue.max_deliver,
        backoff_ms: queue.backoff_ms,
        ack_wait_ms: queue.ack_wait_ms,
        default_deadline_ms: queue.default_deadline_ms,
        update: queue.update,
        key_concurrency: queue.key_concurrency,
        queue: queue.queue,
    }
}

fn normalize_json_queue_binding(
    queue_type: &str,
    value: &Value,
) -> Result<NormalizedJobsQueueBinding, JobsBindingError> {
    let parsed: JobsQueueBindingValue = serde_json::from_value(value.clone()).map_err(|error| {
        JobsBindingError::InvalidQueueBinding {
            queue_type: queue_type.to_string(),
            details: error.to_string(),
        }
    })?;
    if parsed.update.is_some() != parsed.updates_prefix.is_some() {
        return Err(JobsBindingError::InvalidQueueBinding {
            queue_type: queue_type.to_string(),
            details: "update and updatesPrefix must be present together".to_string(),
        });
    }
    Ok(NormalizedJobsQueueBinding {
        queue_type: queue_type.to_string(),
        publish_prefix: parsed.publish_prefix,
        updates_prefix: parsed.updates_prefix,
        work_subject: parsed.work_subject,
        consumer_name: parsed.consumer_name,
        max_deliver: parsed.max_deliver,
        backoff_ms: parsed.backoff_ms,
        ack_wait_ms: parsed.ack_wait_ms,
        default_deadline_ms: parsed.default_deadline_ms,
        update: parsed.update.map(|update| update.schema),
        key_concurrency: parsed.key_concurrency.map(job_key_concurrency_from_value),
        queue: parsed.queue.map(job_queue_depth_from_value),
    })
}

fn job_key_concurrency_from_value(
    value: JobKeyConcurrencyBindingValue,
) -> JobKeyConcurrencyBinding {
    JobKeyConcurrencyBinding {
        key: value.key,
        max_active: value.max_active,
        heartbeat_interval_ms: value.heartbeat_interval_ms,
        heartbeat_ttl_ms: value.heartbeat_ttl_ms,
        stale_policy: match value.stale_policy {
            JobKeyStalePolicyValue::FailStale => JobKeyStalePolicy::FailStale,
            JobKeyStalePolicyValue::Block => JobKeyStalePolicy::Block,
        },
    }
}

fn job_queue_depth_from_value(value: JobQueueDepthBindingValue) -> JobQueueDepthBinding {
    JobQueueDepthBinding {
        max_queued_per_key: value.max_queued_per_key,
        when_full: match value.when_full {
            JobQueueWhenFullValue::Reject => JobQueueWhenFull::Reject,
            JobQueueWhenFullValue::Coalesce => JobQueueWhenFull::Coalesce,
            JobQueueWhenFullValue::ReplaceOldest => JobQueueWhenFull::ReplaceOldest,
        },
    }
}

fn i64_to_u64(value: i64, queue_type: &str, field: &str) -> Result<u64, JobsBindingError> {
    if value < 0 {
        return Err(JobsBindingError::InvalidQueueBinding {
            queue_type: queue_type.to_string(),
            details: format!("{field} must be a non-negative integer"),
        });
    }
    Ok(value as u64)
}
