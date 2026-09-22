use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobContext), "`.")]
pub struct JobContext {
    #[doc = concat!("The `", stringify!(request_id), "` value.")]
    pub request_id: String,
    #[doc = concat!("The `", stringify!(trace_id), "` value.")]
    pub trace_id: String,
    #[doc = concat!("The `", stringify!(traceparent), "` value.")]
    pub traceparent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(tracestate), "` value.")]
    pub tracestate: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[doc = concat!("Public Trellis value set `", stringify!(JobTriggerKind), "`.")]
pub enum JobTriggerKind {
    /// Job was started by a schedule.
    #[serde(rename = "schedule")]
    Schedule,
    /// Job was started by operation control code.
    #[serde(rename = "operation")]
    Operation,
    /// Job was started by an RPC handler.
    #[serde(rename = "rpc")]
    Rpc,
    /// Job was started by an event handler.
    #[serde(rename = "event")]
    Event,
    /// Job was manually replayed by an administrator.
    #[serde(rename = "manualReplay")]
    ManualReplay,
    /// Job was created directly by service code.
    #[serde(rename = "serviceCode")]
    ServiceCode,
    /// Job was created from inside another active job handler.
    #[serde(rename = "parentJob")]
    ParentJob,
}

/// Describes the source that created or retried a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobTrigger), "`.")]
pub struct JobTrigger {
    #[doc = concat!("The `", stringify!(kind), "` value.")]
    pub kind: JobTriggerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(subject), "` value.")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(operation_id), "` value.")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(parent_job_id), "` value.")]
    pub parent_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(trace_id), "` value.")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(request_id), "` value.")]
    pub request_id: Option<String>,
}

/// Relates a job to parent/root work and optional operation context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobLineage), "`.")]
pub struct JobLineage {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(parent_job_id), "` value.")]
    pub parent_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(root_job_id), "` value.")]
    pub root_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(operation_id), "` value.")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(related_keys), "` value.")]
    pub related_keys: Option<Vec<String>>,
}

/// Metadata for an administrator-initiated lifecycle action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobAdminAction), "`.")]
pub struct JobAdminAction {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(reason), "` value.")]
    pub reason: Option<String>,
}

/// Kind of work an active job is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[doc = concat!("Public Trellis value set `", stringify!(JobWaitTargetKind), "`.")]
pub enum JobWaitTargetKind {
    Job,
    Operation,
    External,
}

/// Target of a current or historical active-job wait edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobWaitTarget), "`.")]
pub struct JobWaitTarget {
    #[doc = concat!("The `", stringify!(kind), "` value.")]
    pub kind: JobWaitTargetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(operation_id), "` value.")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(label), "` value.")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(target_type), "` value.")]
    pub target_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(system), "` value.")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(operation), "` value.")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: Option<String>,
}

/// Evidence that an active job is waiting on another unit of work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobWaitEdge), "`.")]
pub struct JobWaitEdge {
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: String,
    #[doc = concat!("The `", stringify!(target), "` value.")]
    pub target: JobWaitTarget,
    #[doc = concat!("The `", stringify!(started_at), "` value.")]
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(label), "` value.")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[doc = concat!("Public Trellis value set `", stringify!(JobState), "`.")]
pub enum JobState {
    Pending,
    Active,
    Retry,
    Completed,
    Failed,
    Cancelled,
    Expired,
    Skipped,
    Stale,
    Dead,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[doc = concat!("Public Trellis value set `", stringify!(JobEventType), "`.")]
pub enum JobEventType {
    Created,
    Started,
    Retry,
    Progress,
    Logged,
    Waiting,
    Resumed,
    Completed,
    Failed,
    Cancelled,
    Expired,
    Skipped,
    Stale,
    Heartbeat,
    #[serde(rename = "staleCompletionIgnored")]
    StaleCompletionIgnored,
    Retried,
    Dead,
    Dismissed,
}

impl JobEventType {
    #[doc = concat!("Trellis API operation `", stringify!(as_token), "`.")]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Started => "started",
            Self::Retry => "retry",
            Self::Progress => "progress",
            Self::Logged => "logged",
            Self::Waiting => "waiting",
            Self::Resumed => "resumed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Skipped => "skipped",
            Self::Stale => "stale",
            Self::Heartbeat => "heartbeat",
            Self::StaleCompletionIgnored => "staleCompletionIgnored",
            Self::Retried => "retried",
            Self::Dead => "dead",
            Self::Dismissed => "dismissed",
        }
    }
}

/// Keyed-concurrency metadata carried by lifecycle events and projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobConcurrency), "`.")]
pub struct JobConcurrency {
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: String,
    #[doc = concat!("The `", stringify!(key_hash), "` value.")]
    pub key_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(instance_id), "` value.")]
    pub instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(slot_token), "` value.")]
    pub slot_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(heartbeat_at), "` value.")]
    pub heartbeat_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(lease_expires_at), "` value.")]
    pub lease_expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(stale_takeover_count), "` value.")]
    pub stale_takeover_count: Option<u64>,
}

/// Queue-policy outcome recorded on keyed lifecycle events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[doc = concat!("Public Trellis value set `", stringify!(JobQueuePolicyOutcome), "`.")]
pub enum JobQueuePolicyOutcome {
    Accepted,
    Rejected,
    Coalesced,
    Replaced,
}

/// Queue-policy metadata carried by lifecycle events and projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobQueuePolicy), "`.")]
pub struct JobQueuePolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(outcome), "` value.")]
    pub outcome: Option<JobQueuePolicyOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(reason), "` value.")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(existing_job_id), "` value.")]
    pub existing_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(replaced_job_id), "` value.")]
    pub replaced_job_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[doc = concat!("Public Trellis value set `", stringify!(JobLogLevel), "`.")]
pub enum JobLogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[doc = concat!("Public Trellis data type `", stringify!(JobLogEntry), "`.")]
pub struct JobLogEntry {
    #[doc = concat!("The `", stringify!(timestamp), "` value.")]
    pub timestamp: String,
    #[doc = concat!("The `", stringify!(level), "` value.")]
    pub level: JobLogLevel,
    #[doc = concat!("The `", stringify!(message), "` value.")]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[doc = concat!("Public Trellis data type `", stringify!(JobProgress), "`.")]
pub struct JobProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(step), "` value.")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(message), "` value.")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(current), "` value.")]
    pub current: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(total), "` value.")]
    pub total: Option<u64>,
}

/// Worker metadata associated with a captured job error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobErrorWorker), "`.")]
pub struct JobErrorWorker {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(instance_id), "` value.")]
    pub instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(version), "` value.")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(runtime), "` value.")]
    pub runtime: Option<String>,
}

/// Structured detail for a job failure or retry reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobErrorDetail), "`.")]
pub struct JobErrorDetail {
    #[doc = concat!("The `", stringify!(message), "` value.")]
    pub message: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(error_type), "` value.")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(stack), "` value.")]
    pub stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(causes), "` value.")]
    pub causes: Option<Vec<JobErrorDetail>>,
    #[doc = concat!("The `", stringify!(fingerprint), "` value.")]
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(first_seen), "` value.")]
    pub first_seen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(occurrence_count), "` value.")]
    pub occurrence_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(worker), "` value.")]
    pub worker: Option<JobErrorWorker>,
}

impl JobErrorDetail {
    /// Create an error detail from the only currently guaranteed handler error shape.
    #[doc = concat!("Trellis API operation `", stringify!(from_message), "`.")]
    pub fn from_message(service: &str, job_type: &str, message: &str) -> Self {
        Self {
            message: message.to_string(),
            error_type: None,
            stack: None,
            causes: None,
            fingerprint: error_fingerprint(service, job_type, message),
            first_seen: None,
            occurrence_count: None,
            worker: None,
        }
    }
}

/// Return the stable fingerprint for a service, job type, and normalized first error line.
#[doc = concat!("Trellis API operation `", stringify!(error_fingerprint), "`.")]
pub fn error_fingerprint(service: &str, job_type: &str, message: &str) -> String {
    let first_line = message.lines().next().unwrap_or_default();
    let normalized = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in format!("{service}\0{job_type}\0{}", normalized.to_lowercase()).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(Job), "`.")]
pub struct Job {
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: String,
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: JobContext,
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[serde(rename = "type")]
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: String,
    #[doc = concat!("The `", stringify!(state), "` value.")]
    pub state: JobState,
    #[doc = concat!("The `", stringify!(payload), "` value.")]
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(result), "` value.")]
    pub result: Option<Value>,
    #[doc = concat!("The `", stringify!(created_at), "` value.")]
    pub created_at: String,
    #[doc = concat!("The `", stringify!(updated_at), "` value.")]
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(started_at), "` value.")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(completed_at), "` value.")]
    pub completed_at: Option<String>,
    #[doc = concat!("The `", stringify!(tries), "` value.")]
    pub tries: u64,
    #[doc = concat!("The `", stringify!(max_tries), "` value.")]
    pub max_tries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(last_error), "` value.")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(error_detail), "` value.")]
    pub error_detail: Option<JobErrorDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(deadline), "` value.")]
    pub deadline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(progress), "` value.")]
    pub progress: Option<JobProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(logs), "` value.")]
    pub logs: Option<Vec<JobLogEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(concurrency), "` value.")]
    pub concurrency: Option<JobConcurrency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(queue_policy), "` value.")]
    pub queue_policy: Option<JobQueuePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(trigger), "` value.")]
    pub trigger: Option<JobTrigger>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(lineage), "` value.")]
    pub lineage: Option<JobLineage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(waiting_on), "` value.")]
    pub waiting_on: Option<Vec<JobWaitEdge>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobEvent), "`.")]
pub struct JobEvent {
    #[doc = concat!("The `", stringify!(job_id), "` value.")]
    pub job_id: String,
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: JobContext,
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: String,
    #[doc = concat!("The `", stringify!(event_type), "` value.")]
    pub event_type: JobEventType,
    #[doc = concat!("The `", stringify!(state), "` value.")]
    pub state: JobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(previous_state), "` value.")]
    pub previous_state: Option<JobState>,
    #[doc = concat!("The `", stringify!(tries), "` value.")]
    pub tries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(max_tries), "` value.")]
    pub max_tries: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(error), "` value.")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(error_detail), "` value.")]
    pub error_detail: Option<JobErrorDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(progress), "` value.")]
    pub progress: Option<JobProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(logs), "` value.")]
    pub logs: Option<Vec<JobLogEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(payload), "` value.")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(result), "` value.")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(deadline), "` value.")]
    pub deadline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(concurrency), "` value.")]
    pub concurrency: Option<JobConcurrency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(queue_policy), "` value.")]
    pub queue_policy: Option<JobQueuePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(trigger), "` value.")]
    pub trigger: Option<JobTrigger>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(lineage), "` value.")]
    pub lineage: Option<JobLineage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(wait_edge), "` value.")]
    pub wait_edge: Option<JobWaitEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(admin_action), "` value.")]
    pub admin_action: Option<JobAdminAction>,
    #[doc = concat!("The `", stringify!(timestamp), "` value.")]
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(WorkerHeartbeat), "`.")]
pub struct WorkerHeartbeat {
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: String,
    #[doc = concat!("The `", stringify!(instance_id), "` value.")]
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(concurrency), "` value.")]
    pub concurrency: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(version), "` value.")]
    pub version: Option<String>,
    #[doc = concat!("The `", stringify!(timestamp), "` value.")]
    pub timestamp: String,
}
