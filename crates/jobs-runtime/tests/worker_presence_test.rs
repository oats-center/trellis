use time::OffsetDateTime;
use trellis_jobs_runtime::worker_presence::{
    reduce_worker_presence, worker_presence_from_heartbeat, worker_presence_is_fresh,
    WorkerPresenceRecord,
};
use trellis_rs::jobs::types::WorkerHeartbeat;

fn sample_heartbeat(timestamp: &str) -> WorkerHeartbeat {
    WorkerHeartbeat {
        service: "documents".to_string(),
        job_type: "document-process".to_string(),
        instance_id: "instance-1".to_string(),
        concurrency: Some(2),
        version: Some("0.6.1".to_string()),
        timestamp: timestamp.to_string(),
    }
}

#[test]
fn worker_presence_record_maps_from_heartbeat() {
    let record = worker_presence_from_heartbeat(&sample_heartbeat("2026-03-30T12:00:00Z"));
    assert_eq!(record.service, "documents");
    assert_eq!(record.job_type, "document-process");
    assert_eq!(record.instance_id, "instance-1");
    assert_eq!(record.concurrency, Some(2));
    assert_eq!(record.version.as_deref(), Some("0.6.1"));
    assert_eq!(record.heartbeat_at, "2026-03-30T12:00:00Z");
}

#[test]
fn reduce_worker_presence_keeps_newer_heartbeat() {
    let current = WorkerPresenceRecord {
        service: "documents".to_string(),
        job_type: "document-process".to_string(),
        instance_id: "instance-1".to_string(),
        concurrency: Some(1),
        version: None,
        heartbeat_at: "2026-03-30T12:00:00Z".to_string(),
    };
    let next = worker_presence_from_heartbeat(&sample_heartbeat("2026-03-30T12:00:30Z"));

    let reduced = reduce_worker_presence(Some(&current), &next).expect("newer heartbeat wins");
    assert_eq!(reduced.heartbeat_at, "2026-03-30T12:00:30Z");
    assert_eq!(reduced.concurrency, Some(2));
}

#[test]
fn reduce_worker_presence_ignores_older_heartbeat() {
    let current = WorkerPresenceRecord {
        service: "documents".to_string(),
        job_type: "document-process".to_string(),
        instance_id: "instance-1".to_string(),
        concurrency: Some(2),
        version: Some("0.6.1".to_string()),
        heartbeat_at: "2026-03-30T12:00:30Z".to_string(),
    };
    let older = worker_presence_from_heartbeat(&sample_heartbeat("2026-03-30T12:00:00Z"));

    assert!(reduce_worker_presence(Some(&current), &older).is_none());
}

#[test]
fn worker_presence_freshness_uses_timestamp_window() {
    let fresh = worker_presence_from_heartbeat(&sample_heartbeat("2026-03-30T12:00:30Z"));
    let stale = worker_presence_from_heartbeat(&sample_heartbeat("2026-03-30T11:58:00Z"));
    let now = OffsetDateTime::parse(
        "2026-03-30T12:01:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .expect("parse now timestamp");

    assert!(worker_presence_is_fresh(&fresh, now));
    assert!(!worker_presence_is_fresh(&stale, now));
}
