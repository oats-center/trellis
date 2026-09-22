use crate::jobs::types::JobEventType;

pub const JOBS_PREFIX: &str = "trellis.jobs";
pub const WORK_PREFIX: &str = "trellis.work";
pub const JOBS_WILDCARD: &str = "trellis.jobs.>";
pub const WORKER_HEARTBEATS_WILDCARD: &str = "trellis.jobs.workers.>";

#[doc = concat!("Trellis API operation `", stringify!(job_event_subject), "`.")]
pub fn job_event_subject(
    service: &str,
    job_type: &str,
    job_id: &str,
    event: JobEventType,
) -> String {
    format!(
        "{JOBS_PREFIX}.{service}.{job_type}.{job_id}.{}",
        event.as_token()
    )
}

#[doc = concat!("Trellis API operation `", stringify!(work_subject), "`.")]
pub fn work_subject(service: &str, job_type: &str) -> String {
    format!("{WORK_PREFIX}.{service}.{job_type}")
}

#[doc = concat!("Trellis API operation `", stringify!(worker_heartbeat_subject), "`.")]
pub fn worker_heartbeat_subject(service: &str, job_type: &str, instance_id: &str) -> String {
    format!("{JOBS_PREFIX}.workers.{service}.{job_type}.{instance_id}.heartbeat")
}
