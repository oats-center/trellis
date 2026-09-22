use serde_json::{json, Value};
use trellis_rs::{generated::Codec, jobs::types::Job};
use trellis_runtime_apis::types::{
    Bytes, JobsCancelResponsejob, JobsDismissDLQResponsejob, JobsInspectResponsejob,
    JobsListDLQResponseentriesItem, JobsListServicesResponseentriesItemworkersItem,
    JobsReplayDLQResponsejob, JobsRetryResponsejob,
};

use crate::storage::JobProjectionMetadata;
use crate::worker_presence::WorkerPresenceRecord;

use super::JobsQueryError;

pub(super) fn worker_presence_to_wire(
    worker: &WorkerPresenceRecord,
) -> JobsListServicesResponseentriesItemworkersItem {
    decode_wire(
        json!({
            "concurrency": worker.concurrency,
            "instanceId": worker.instance_id,
            "jobType": worker.job_type,
            "service": worker.service,
            "timestamp": worker.heartbeat_at,
            "version": worker.version,
        }),
        "worker presence",
    )
    .expect("stored worker presence must satisfy the generated Jobs API")
}

macro_rules! job_mapper {
    ($name:ident, $type:ty, $model:literal) => {
        pub(super) fn $name(
            job: &Job,
            metadata: &JobProjectionMetadata,
        ) -> Result<$type, JobsQueryError> {
            decode_wire(job_wire_value(job, metadata)?, $model)
        }
    };
}

job_mapper!(
    job_to_dlq_item,
    JobsListDLQResponseentriesItem,
    "job dlq list item"
);
job_mapper!(
    job_to_inspect_item,
    JobsInspectResponsejob,
    "job inspect response"
);
job_mapper!(
    job_to_cancel_item,
    JobsCancelResponsejob,
    "job cancel response"
);
job_mapper!(
    job_to_retry_item,
    JobsRetryResponsejob,
    "job retry response"
);
job_mapper!(
    job_to_replay_item,
    JobsReplayDLQResponsejob,
    "job replay dlq response"
);
job_mapper!(
    job_to_dismiss_item,
    JobsDismissDLQResponsejob,
    "job dismiss dlq response"
);

fn job_wire_value(job: &Job, metadata: &JobProjectionMetadata) -> Result<Value, JobsQueryError> {
    let mut value =
        serde_json::to_value(job).map_err(|error| JobsQueryError::ConvertWireModel {
            model: "job",
            details: error.to_string(),
        })?;
    let object = value
        .as_object_mut()
        .expect("Job always serializes as an object");

    object.insert("payload".to_string(), encode_document(&job.payload)?);
    object.insert(
        "result".to_string(),
        job.result
            .as_ref()
            .map(encode_document)
            .transpose()?
            .unwrap_or(Value::Null),
    );
    object.insert(
        "concurrency".to_string(),
        metadata.concurrency.as_ref().map_or(Value::Null, |value| {
            json!({
                "heartbeatAt": value.heartbeat_at,
                "key": value.key,
                "keyHash": value.key_hash,
                "leaseExpiresAt": value.lease_expires_at,
                "staleTakeoverCount": value.stale_takeover_count,
            })
        }),
    );
    object.insert(
        "queuePolicy".to_string(),
        metadata.queue_policy.as_ref().map_or(Value::Null, |value| {
            json!({
                "existingJobId": value.existing_job_id,
                "outcome": value.outcome,
                "reason": value.reason,
                "replacedJobId": value.replaced_job_id,
            })
        }),
    );
    Ok(value)
}

pub(super) fn encode_document(value: &Value) -> Result<Value, JobsQueryError> {
    let bytes = serde_json::to_vec(value).map_err(|error| JobsQueryError::ConvertWireModel {
        model: "JSON document bytes",
        details: error.to_string(),
    })?;
    Codec::encode(&Bytes(bytes)).map_err(|error| JobsQueryError::ConvertWireModel {
        model: "JSON document bytes",
        details: error.to_string(),
    })
}

pub(super) fn decode_wire<T: Codec>(
    mut value: Value,
    model: &'static str,
) -> Result<T, JobsQueryError> {
    stringify_integers(&mut value);
    T::decode(value).map_err(|error| JobsQueryError::ConvertWireModel {
        model,
        details: error.to_string(),
    })
}

fn stringify_integers(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(stringify_integers),
        Value::Object(values) => values.values_mut().for_each(stringify_integers),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            *value = Value::String(number.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_rs::jobs::types::{JobContext, JobProgress, JobState};

    use super::*;

    fn job_with_progress(progress: JobProgress) -> Job {
        Job {
            id: "job-1".to_string(),
            context: JobContext {
                request_id: "request-job-1".to_string(),
                trace_id: "0123456789abcdef0123456789abcdef".to_string(),
                traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
                tracestate: None,
            },
            service: "documents".to_string(),
            job_type: "document-process".to_string(),
            state: JobState::Active,
            payload: json!({ "documentId": "doc-1" }),
            result: None,
            created_at: "2026-03-28T12:00:00.000Z".to_string(),
            updated_at: "2026-03-28T12:01:00.000Z".to_string(),
            started_at: Some("2026-03-28T12:00:30.000Z".to_string()),
            completed_at: None,
            tries: 1,
            max_tries: 5,
            last_error: None,
            error_detail: None,
            deadline: None,
            progress: Some(progress),
            logs: None,
            concurrency: None,
            queue_policy: None,
            trigger: None,
            lineage: None,
            waiting_on: None,
        }
    }

    #[test]
    fn job_progress_wire_mapping_allows_message_only_progress() {
        let job = job_with_progress(JobProgress {
            step: None,
            message: Some("Scanning".to_string()),
            current: None,
            total: None,
        });

        let item = job_to_inspect_item(&job, &JobProjectionMetadata::default()).expect("map job");
        let progress = item.progress.as_ref().expect("progress should be present");

        assert_eq!(progress.message.as_deref(), Some("Scanning"));
        assert_eq!(progress.current, None);
        assert_eq!(progress.total, None);
    }

    #[test]
    fn job_progress_wire_mapping_preserves_optional_counts() {
        let job = job_with_progress(JobProgress {
            step: Some("scan".to_string()),
            message: Some("Scanning".to_string()),
            current: Some(2),
            total: Some(10),
        });

        let item = job_to_inspect_item(&job, &JobProjectionMetadata::default()).expect("map job");
        let progress = item.progress.as_ref().expect("progress should be present");

        assert_eq!(progress.step.as_deref(), Some("scan"));
        assert_eq!(progress.message.as_deref(), Some("Scanning"));
        assert_eq!(progress.current.map(|value| value.0), Some(2));
        assert_eq!(progress.total.map(|value| value.0), Some(10));
        assert_eq!(
            serde_json::from_slice::<Value>(&item.payload.0).expect("decode payload JSON"),
            json!({ "documentId": "doc-1" })
        );
        let encoded = item.encode().expect("encode generated item");
        assert_eq!(encoded.get("maxTries"), Some(&json!("5")));
    }
}
