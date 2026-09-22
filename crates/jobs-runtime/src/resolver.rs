use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use futures_util::StreamExt;
use trellis_rs::jobs::keys::JobKeyState;

/// One retained Core-managed Jobs queue binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobResourceBinding {
    pub service: String,
    pub job_type: String,
    pub stream: String,
    pub consumer: String,
}

/// Authoritative keyed coordinator state and the Jobs stream horizon captured after it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobKeySnapshot {
    pub state: JobKeyState,
    pub jobs_horizon: u64,
}

/// Core catalog and coordinator lookup used by Jobs advisory and query paths.
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

/// Jobs resolver backed by Core's retained SQLite resource evidence and real NATS resources.
#[derive(Clone, Debug)]
pub struct SqliteJobResourceResolver {
    path: PathBuf,
    nats: async_nats::Client,
    jobs_stream: String,
}

impl SqliteJobResourceResolver {
    /// Create a resolver for one Core catalog and Jobs lifecycle stream.
    pub fn new(path: PathBuf, nats: async_nats::Client, jobs_stream: String) -> Self {
        Self {
            path,
            nats,
            jobs_stream,
        }
    }
}

impl JobResourceResolver for SqliteJobResourceResolver {
    fn by_consumer<'a>(
        &'a self,
        stream: &'a str,
        consumer: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobResourceBinding>, String>> + Send + 'a>> {
        let path = self.path.clone();
        let physical = (stream.to_owned(), consumer.to_owned());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                lookup_job_binding(&path, None, Some(&physical), true)
                    .map(|binding| binding.map(|(binding, _)| binding))
            })
            .await
            .map_err(|error| error.to_string())?
        })
    }

    fn key_snapshot<'a>(
        &'a self,
        service: &'a str,
        job_type: &'a str,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JobKeySnapshot>, String>> + Send + 'a>> {
        let path = self.path.clone();
        let nats = self.nats.clone();
        let jobs_stream = self.jobs_stream.clone();
        let logical = (service.to_owned(), job_type.to_owned());
        let key = key.to_owned();
        Box::pin(async move {
            let binding = tokio::task::spawn_blocking(move || {
                lookup_job_binding(&path, Some(&logical), None, false)
            })
            .await
            .map_err(|error| error.to_string())??;
            let Some((_, namespace)) = binding else {
                return Ok(None);
            };
            let jetstream = async_nats::jetstream::new(nats);
            let store = jetstream
                .get_key_value(format!("JOBS_KEYS_{namespace}"))
                .await
                .map_err(|error| error.to_string())?;
            let mut keys = store.keys().await.map_err(|error| error.to_string())?;
            let mut state = None;
            while let Some(candidate) = keys.next().await {
                let candidate = candidate.map_err(|error| error.to_string())?;
                let Some(entry) = store
                    .entry(candidate)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    continue;
                };
                let candidate: JobKeyState =
                    serde_json::from_slice(&entry.value).map_err(|error| error.to_string())?;
                if candidate.key == key && candidate.job_type == job_type {
                    state = Some(candidate);
                    break;
                }
            }
            let Some(state) = state else { return Ok(None) };
            let mut stream = jetstream
                .get_stream(jobs_stream)
                .await
                .map_err(|error| error.to_string())?;
            let jobs_horizon = stream
                .info()
                .await
                .map_err(|error| error.to_string())?
                .state
                .last_sequence;
            Ok(Some(JobKeySnapshot {
                state,
                jobs_horizon,
            }))
        })
    }
}

fn lookup_job_binding(
    path: &Path,
    logical: Option<&(String, String)>,
    physical: Option<&(String, String)>,
    include_retained: bool,
) -> Result<Option<(JobResourceBinding, String)>, String> {
    let connection = rusqlite::Connection::open(path).map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT participant_id,local_name,provider_identity,state
         FROM auth_resource_binding_evidence WHERE resource_kind='jobQueue'",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (service, job_type, provider, state) = row.map_err(|error| error.to_string())?;
        if state != "available" && !(include_retained && state == "stale") {
            continue;
        }
        let provider: serde_json::Value =
            serde_json::from_str(&provider).map_err(|error| error.to_string())?;
        let Some(namespace) = provider
            .get("namespace")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(stream) = provider
            .get("work_stream")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(consumer) = provider.get("consumer").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if logical.is_some_and(|wanted| wanted.0 != service || wanted.1 != job_type)
            || physical.is_some_and(|wanted| wanted.0 != stream || wanted.1 != consumer)
        {
            continue;
        }
        return Ok(Some((
            JobResourceBinding {
                service,
                job_type,
                stream: stream.to_owned(),
                consumer: consumer.to_owned(),
            },
            namespace.to_owned(),
        )));
    }
    Ok(None)
}
