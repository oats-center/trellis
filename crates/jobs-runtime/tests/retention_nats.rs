//! Jobs retention against a real broker, lifecycle projector, and SQLite.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, stream};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_jobs_runtime::{
    start_jobs_projector, JobKeySnapshot, JobResourceBinding, JobResourceResolver, JobsQuery,
    SqliteJobsStore,
};
use trellis_rs::__internal::FromProviderNats as _;
use trellis_rs::jobs::events::{created, EventMeta};
use trellis_rs::jobs::types::{Job, JobContext, JobState};
use trellis_rs::jobs::JobsRuntime;

mod common;
use common::start_nats;

const PREFIX: &str = "trellis.jobs.namespace.queue";

struct Resolver;

impl JobResourceResolver for Resolver {
    fn by_consumer<'a>(
        &'a self,
        _stream: &'a str,
        _consumer: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobResourceBinding>, String>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }

    fn by_publish_prefix<'a>(
        &'a self,
        publish_prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobResourceBinding>, String>> + Send + 'a>> {
        Box::pin(async move {
            Ok((publish_prefix == PREFIX).then(|| JobResourceBinding {
                service: "documents".to_owned(),
                job_type: "process".to_owned(),
                stream: "JOBS_WORK".to_owned(),
                consumer: "documents-process".to_owned(),
            }))
        })
    }

    fn key_snapshot<'a>(
        &'a self,
        _service: &'a str,
        _job_type: &'a str,
        _key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobKeySnapshot>, String>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }
}

/// A job submitted without a deadline whose workers never come must not stay
/// pending after the broker drops its only queued work: retention records it
/// as expired, and leaves a job still inside the window alone.
#[tokio::test]
async fn retention_expires_jobs_no_worker_claimed() {
    let (_server, _directory, client) = start_nats().await;
    let context = jetstream::new(client.clone());
    context
        .create_stream(stream::Config {
            name: "JOBS".to_owned(),
            subjects: vec!["trellis.jobs.>".to_owned()],
            storage: stream::StorageType::Memory,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create lifecycle stream");
    // The work queue drops unclaimed work after a second, as the configured
    // history retention does after its days.
    let mut work = context
        .create_stream(stream::Config {
            name: "JOBS_WORK".to_owned(),
            subjects: vec!["trellis.work.>".to_owned()],
            retention: stream::RetentionPolicy::WorkQueue,
            storage: stream::StorageType::Memory,
            max_age: Duration::from_secs(1),
            sources: Some(vec![stream::Source {
                name: "JOBS".to_owned(),
                subject_transforms: vec![stream::SubjectTransform {
                    source: "trellis.jobs.*.*.*.created".to_owned(),
                    destination: "trellis.work.$1.$2".to_owned(),
                }],
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await
        .expect("create work stream");

    let jobs_runtime = JobsRuntime::from_nats(client.clone());
    let store = SqliteJobsStore::open_in_memory().expect("open projection");
    let resolver: Arc<dyn JobResourceResolver> = Arc::new(Resolver);
    let projector = start_jobs_projector(
        jobs_runtime.clone(),
        store.clone(),
        "JOBS".to_owned(),
        Arc::clone(&resolver),
    )
    .await
    .expect("start projector");
    let job_context = JobContext {
        request_id: "request-1".to_owned(),
        trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
        traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_owned(),
        tracestate: None,
    };
    let submit = |id: &'static str| {
        let jobs_runtime = jobs_runtime.clone();
        let job_context = job_context.clone();
        async move {
            let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
            let event = created(
                EventMeta {
                    service: "documents",
                    job_type: "process",
                    job_id: id,
                    context: &job_context,
                    timestamp: &now,
                },
                json!({ "document": id }),
                3,
                None,
            );
            jobs_runtime
                .publish_event(format!("{PREFIX}.{id}.created"), &event)
                .await
                .expect("submit job");
        }
    };

    submit("abandoned").await;
    let abandoned = wait_for_job(&store, "abandoned", JobState::Pending).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while work.info().await.unwrap().state.messages == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        while work.info().await.unwrap().state.messages > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the broker drops the unclaimed work");

    submit("recent").await;
    let recent = wait_for_job(&store, "recent", JobState::Pending).await;

    // A retention whose cutoff falls between the two submissions.
    let abandoned_at = OffsetDateTime::parse(&abandoned.updated_at, &Rfc3339).unwrap();
    let recent_at = OffsetDateTime::parse(&recent.updated_at, &Rfc3339).unwrap();
    let cutoff = abandoned_at + (recent_at - abandoned_at) / 2;
    let history: time::Duration = OffsetDateTime::now_utc() - cutoff;
    let history = history.unsigned_abs();
    let query = JobsQuery::with_store(jobs_runtime, store.clone(), resolver);
    let pass = query.apply_retention(history).await.expect("retention");
    assert_eq!(pass.expired, 1);

    let expired = wait_for_job(&store, "abandoned", JobState::Expired).await;
    assert!(expired.last_error.is_some(), "the expiry says why");
    assert_eq!(
        store
            .get_job("documents", "process", "recent")
            .unwrap()
            .unwrap()
            .state,
        JobState::Pending
    );
    projector.stop().await;
}

async fn wait_for_job(store: &SqliteJobsStore, id: &str, state: JobState) -> Job {
    for _ in 0..200 {
        if let Some(job) = store
            .get_job("documents", "process", id)
            .expect("read projection")
            .filter(|job| job.state == state)
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("job {id} did not reach {state:?}");
}
