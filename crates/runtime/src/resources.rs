use std::time::Duration;

use async_nats::jetstream::{self, stream};

use crate::health::{DEFAULT_TRANSPORT_RETENTION_HOURS, HEALTH_STREAM, HEALTH_SUBJECT};
use crate::supervisor::RuntimeError;
use crate::{RuntimeConfig, RuntimeMode, SubsystemName};
use trellis_events_runtime::{DLQ_STREAM, DLQ_SUBJECTS, REPLAY_STREAM, REPLAY_SUBJECTS};

pub(crate) const EVENT_STREAM: &str = "trellis";
pub(crate) const EVENT_STREAM_RETENTION: Duration =
    days(crate::config::EVENT_STREAM_RETENTION_DAYS);
pub(crate) const JOBS_STREAM: &str = "JOBS";
pub(crate) const JOBS_WORK_STREAM: &str = "JOBS_WORK";
pub(crate) const JOBS_ADVISORIES_STREAM: &str = "JOBS_ADVISORIES";

/// How long the Jobs streams keep a job past `[jobs] history_retention_days`.
///
/// The hourly Jobs retention pass expires a job no worker claimed within the
/// retention; the margin lets that record land while the job's work and its
/// lifecycle are still in the streams, even across a day of Jobs downtime.
pub(crate) const JOBS_STREAM_GRACE: Duration = days(1);

pub(crate) const fn days(days: u32) -> Duration {
    Duration::from_secs(days as u64 * 24 * 60 * 60)
}

#[derive(Clone, Debug)]
pub(crate) struct ExpectedRuntimeResources {
    streams: Vec<stream::Config>,
}

impl ExpectedRuntimeResources {
    pub(crate) fn for_mode(
        mode: RuntimeMode,
        config: &RuntimeConfig,
        storage: stream::StorageType,
    ) -> Self {
        let subsystems = mode.subsystems();
        let mut streams = Vec::new();
        if subsystems.contains(&SubsystemName::Platform)
            || subsystems.contains(&SubsystemName::Events)
            || subsystems.contains(&SubsystemName::Health)
        {
            streams.push(event_stream_config());
        }
        if subsystems.contains(&SubsystemName::Events) {
            streams.extend(events_stream_configs(config));
        }
        if subsystems.contains(&SubsystemName::Jobs) {
            streams.extend(jobs_stream_configs(config));
            if !subsystems.contains(&SubsystemName::Events) {
                streams.push(advisory_stream_config());
            }
        }
        if subsystems.contains(&SubsystemName::Health) {
            streams.push(health_stream_config(config));
        }
        for stream in &mut streams {
            stream.storage = storage;
        }
        Self { streams }
    }

    pub(crate) fn streams(&self) -> &[stream::Config] {
        &self.streams
    }

    /// Creates each missing stream and converges each existing one, in order.
    ///
    /// A stream JetStream refuses for lack of room is reported through
    /// `shortages`, with the numbers, and fails with
    /// [`RuntimeError::JetStreamExhausted`], which the caller retries.
    pub(crate) async fn converge_streams(
        &self,
        client: async_nats::Client,
        shortages: &crate::capacity::Shortages,
    ) -> Result<(), RuntimeError> {
        let jetstream = jetstream::new(client.clone());
        for expected in &self.streams {
            let resource = format!("stream {}", expected.name);
            match Self::converge_stream(&jetstream, expected).await {
                Ok(()) => shortages.recovered(&resource),
                Err(RuntimeError::Nats(error)) => {
                    let Some(exhaustion) = crate::capacity::exhaustion_in(&error) else {
                        return Err(RuntimeError::Nats(error));
                    };
                    let wanted = u64::try_from(expected.max_bytes)
                        .ok()
                        .filter(|bytes| *bytes > 0);
                    return Err(RuntimeError::JetStreamExhausted(
                        shortages
                            .refused(&client, &resource, exhaustion, wanted, &error)
                            .await,
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    async fn converge_stream(
        jetstream: &jetstream::Context,
        expected: &stream::Config,
    ) -> Result<(), RuntimeError> {
        match jetstream.get_stream(&expected.name).await {
            Ok(mut stream) => {
                let info = stream
                    .info()
                    .await
                    .map_err(|error| RuntimeError::Nats(error.to_string()))?;
                if let Some(update) = stream_update(&info.config, expected).map_err(|reason| {
                    RuntimeError::IncompatibleResource {
                        name: expected.name.clone(),
                        reason,
                    }
                })? {
                    jetstream
                        .update_stream(update)
                        .await
                        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
                }
            }
            Err(_) => {
                jetstream
                    .create_stream(expected.clone())
                    .await
                    .map_err(|error| RuntimeError::Nats(error.to_string()))?;
            }
        }
        Ok(())
    }
}

fn stream_update(
    actual: &stream::Config,
    expected: &stream::Config,
) -> Result<Option<stream::Config>, &'static str> {
    if actual.name != expected.name
        || actual.subjects != expected.subjects
        || actual.retention != expected.retention
        || actual.discard != expected.discard
        || actual.sources != expected.sources
    {
        return Err("stream identity, subjects, retention, or discard policy differs");
    }

    if matches!(
        expected.name.as_str(),
        JOBS_ADVISORIES_STREAM | REPLAY_STREAM | DLQ_STREAM
    ) && actual.discard_new_per_subject != expected.discard_new_per_subject
    {
        return Err("per-subject discard policy differs");
    }

    if !matches!(
        expected.name.as_str(),
        EVENT_STREAM
            | HEALTH_STREAM
            | JOBS_STREAM
            | JOBS_WORK_STREAM
            | JOBS_ADVISORIES_STREAM
            | REPLAY_STREAM
            | DLQ_STREAM
    ) {
        return Err("stream is not owned by the runtime");
    }

    // The configured limits are the retention policy: an existing stream
    // takes them in place in either direction, and JetStream removes only what
    // the new policy no longer keeps.
    // JetStream reports an unset count or size limit as -1.
    let unlimited = |limit: i64| if limit == 0 { -1 } else { limit };
    let mut update = actual.clone();
    update.max_age = expected.max_age;
    update.max_messages = unlimited(expected.max_messages);
    update.max_messages_per_subject = unlimited(expected.max_messages_per_subject);
    update.max_bytes = unlimited(expected.max_bytes);
    let mut changed = update.max_age != actual.max_age
        || update.max_messages != actual.max_messages
        || update.max_messages_per_subject != actual.max_messages_per_subject
        || update.max_bytes != actual.max_bytes;

    if actual.allow_direct != expected.allow_direct {
        if expected.allow_direct {
            update.allow_direct = true;
            changed = true;
        } else {
            return Err("direct access cannot be disabled");
        }
    }

    Ok(changed.then_some(update))
}

pub(crate) fn stream_is_compatible(actual: &stream::Config, expected: &stream::Config) -> bool {
    stream_update(actual, expected).is_ok()
}

fn event_stream_config() -> stream::Config {
    stream::Config {
        name: EVENT_STREAM.to_owned(),
        subjects: vec!["events.>".to_owned()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::Old,
        max_age: EVENT_STREAM_RETENTION,
        ..Default::default()
    }
}

/// The `JOBS` lifecycle and `JOBS_WORK` streams keep `[jobs]
/// history_retention_days` plus [`JOBS_STREAM_GRACE`]. Settled jobs live on in
/// the Jobs projection, and replay of a dead job reads its payload from there,
/// so the age bounds only stream-read history and work nobody consumed; the
/// Jobs retention pass expires such a job before the grace runs out.
fn jobs_stream_configs(config: &RuntimeConfig) -> [stream::Config; 2] {
    let max_age = days(config.jobs_history_retention_days()) + JOBS_STREAM_GRACE;
    [
        stream::Config {
            name: JOBS_STREAM.to_owned(),
            subjects: vec!["trellis.jobs.>".to_owned()],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::Old,
            max_age,
            max_messages: -1,
            max_messages_per_subject: -1,
            max_bytes: -1,
            allow_direct: true,
            ..Default::default()
        },
        stream::Config {
            name: JOBS_WORK_STREAM.to_owned(),
            subjects: vec!["trellis.work.>".to_owned()],
            retention: stream::RetentionPolicy::WorkQueue,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::Old,
            max_age,
            max_messages: -1,
            max_messages_per_subject: -1,
            max_bytes: -1,
            allow_direct: true,
            sources: Some(vec![stream::Source {
                name: JOBS_STREAM.to_owned(),
                subject_transforms: vec![
                    stream::SubjectTransform {
                        source: "trellis.jobs.*.*.*.created".to_owned(),
                        destination: "trellis.work.$1.$2".to_owned(),
                    },
                    stream::SubjectTransform {
                        source: "trellis.jobs.*.*.*.retried".to_owned(),
                        destination: "trellis.work.$1.$2".to_owned(),
                    },
                ],
                ..Default::default()
            }]),
            ..Default::default()
        },
    ]
}

/// The dead-letter journal keeps every open entry: its revision-one record
/// holds the only copy of the original evidence replay needs, so it has no age
/// or size limit. Settled entries are purged by the Events retention loop. A
/// replay envelope keeps `[events] dead_letter_retention_days`.
fn events_stream_configs(config: &RuntimeConfig) -> [stream::Config; 3] {
    [
        stream::Config {
            name: DLQ_STREAM.to_owned(),
            subjects: vec![DLQ_SUBJECTS.to_owned()],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::New,
            max_messages: -1,
            max_messages_per_subject: -1,
            max_bytes: -1,
            allow_direct: true,
            ..Default::default()
        },
        stream::Config {
            name: REPLAY_STREAM.to_owned(),
            subjects: vec![REPLAY_SUBJECTS.to_owned()],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::New,
            max_age: days(config.dead_letter_retention_days()),
            max_messages: -1,
            max_messages_per_subject: 1,
            max_bytes: -1,
            discard_new_per_subject: true,
            allow_direct: true,
            ..Default::default()
        },
        advisory_stream_config(),
    ]
}

/// A max-deliveries advisory is useful only while the original event it names
/// is still in the event stream, so it keeps the event stream's retention.
fn advisory_stream_config() -> stream::Config {
    stream::Config {
        name: JOBS_ADVISORIES_STREAM.to_owned(),
        subjects: vec!["$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>".to_owned()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::New,
        max_age: EVENT_STREAM_RETENTION,
        max_messages: -1,
        max_messages_per_subject: -1,
        max_bytes: -1,
        ..Default::default()
    }
}

/// The Health stream keeps `[health] transport_retention_hours` of
/// heartbeats. It has no size bound unless `transport_max_bytes` sets one:
/// JetStream reserves a bound against the broker's storage whether or not the
/// stream holds anything, and every heartbeat it holds is also in the event
/// stream, which keeps them longer with no size bound.
fn health_stream_config(config: &RuntimeConfig) -> stream::Config {
    let health = config.health.as_ref();
    let max_age = Duration::from_secs(
        health
            .and_then(|health| health.transport_retention_hours)
            .map(u64::from)
            .unwrap_or(DEFAULT_TRANSPORT_RETENTION_HOURS)
            * 60
            * 60,
    );
    let max_bytes = health
        .and_then(|health| health.transport_max_bytes)
        .map_or(-1, |bytes| i64::try_from(bytes).unwrap_or(i64::MAX));
    stream::Config {
        name: HEALTH_STREAM.to_owned(),
        sources: Some(vec![stream::Source {
            name: EVENT_STREAM.into(),
            filter_subject: Some(HEALTH_SUBJECT.into()),
            ..Default::default()
        }]),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::Old,
        max_age,
        max_bytes,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt as _;

    async fn start_nats(
        directory: &std::path::Path,
    ) -> (tokio::process::Child, async_nats::Client) {
        // Held until the broker's listener is up; see ReservedPort.
        let reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
        let port = reservation.port();
        let binary =
            std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
        let mut server = tokio::process::Command::new(binary)
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(directory)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("producer-resolved NATS executable");
        let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(line) = output.next_line().await.unwrap() {
                if line.contains("Server is ready") {
                    return;
                }
            }
            panic!("NATS exited before readiness");
        })
        .await
        .expect("NATS readiness deadline");
        let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
            .await
            .unwrap();
        (server, nats)
    }

    async fn info(jetstream: &jetstream::Context, name: &str) -> stream::Info {
        jetstream
            .get_stream(name)
            .await
            .unwrap()
            .info()
            .await
            .unwrap()
            .clone()
    }

    #[tokio::test]
    async fn an_existing_install_takes_the_retention_policy_and_reports_its_storage() {
        let directory = tempfile::tempdir().unwrap();
        let (_server, nats) = start_nats(directory.path()).await;
        let jetstream = jetstream::new(nats.clone());
        let resources = |toml: &str| {
            ExpectedRuntimeResources::for_mode(
                RuntimeMode::All,
                &RuntimeConfig::from_toml_str(toml).unwrap(),
                stream::StorageType::File,
            )
        };

        // An installation from before retention defaults: unbounded lifecycle
        // and advisory streams that already hold data.
        let mut legacy = resources("");
        for stream in &mut legacy.streams {
            if matches!(stream.name.as_str(), JOBS_STREAM | JOBS_ADVISORIES_STREAM) {
                stream.max_age = Duration::ZERO;
            }
        }
        legacy
            .converge_streams(nats.clone(), &crate::capacity::Shortages::default())
            .await
            .unwrap();
        jetstream
            .publish("trellis.jobs.namespace.queue.job.created", "job".into())
            .await
            .unwrap()
            .await
            .unwrap();
        for _ in 0..20 {
            jetstream
                .publish(HEALTH_SUBJECT, vec![b'h'; 200].into())
                .await
                .unwrap()
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while info(&jetstream, HEALTH_STREAM).await.state.messages < 20 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("health stream sources every heartbeat");

        let policy = "[jobs]\nhistory_retention_days = 3\n[health]\ntransport_max_bytes = 2048\n";
        resources(policy)
            .converge_streams(nats.clone(), &crate::capacity::Shortages::default())
            .await
            .unwrap();

        let jobs = info(&jetstream, JOBS_STREAM).await;
        assert_eq!(jobs.config.max_age, days(3) + JOBS_STREAM_GRACE);
        assert_eq!(
            jobs.state.messages, 1,
            "no job is older than the new policy"
        );
        assert_eq!(
            info(&jetstream, JOBS_WORK_STREAM).await.config.max_age,
            days(3) + JOBS_STREAM_GRACE
        );
        assert_eq!(
            info(&jetstream, JOBS_ADVISORIES_STREAM)
                .await
                .config
                .max_age,
            EVENT_STREAM_RETENTION
        );
        assert_eq!(
            info(&jetstream, REPLAY_STREAM).await.config.max_age,
            days(crate::config::DEFAULT_DEAD_LETTER_RETENTION_DAYS)
        );
        let dlq = info(&jetstream, DLQ_STREAM).await;
        assert_eq!(
            (dlq.config.max_age, dlq.config.max_bytes),
            (Duration::ZERO, -1),
            "the journal keeps every open dead letter"
        );
        // A smaller size policy is applied in place: the oldest heartbeats
        // go, the newest stay.
        let health = info(&jetstream, HEALTH_STREAM).await;
        assert_eq!(health.config.max_bytes, 2048);
        assert!(health.state.messages > 0 && health.state.messages < 20);
        assert_eq!(health.state.last_sequence, 20);

        // The storage report reads the same streams and the databases on disk.
        let data = directory.path().join("data");
        let config = RuntimeConfig::from_toml_str(&format!(
            "[events.storage]\nkind = \"sqlite\"\npath = \"{events}\"\n\
            [jobs]\nhistory_retention_days = 3\n\
            [jobs.storage]\nkind = \"sqlite\"\npath = \"{jobs}\"\n",
            events = data.join("events.sqlite").display(),
            jobs = data.join("jobs.sqlite").display(),
        ))
        .unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let events = trellis_events_runtime::EventsStore::open(
            data.join("events.sqlite"),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        trellis_jobs_runtime::SqliteJobsStore::open(
            data.join("jobs.sqlite"),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let class = |classes: &[crate::health::storage::StorageClass], name: &str| {
            classes
                .iter()
                .find(|class| class.name == name)
                .cloned()
                .unwrap_or_else(|| panic!("{name} is reported"))
        };
        let empty = crate::health::storage::collect(&config, &nats).await;
        assert_eq!(class(&empty, "recordedEvents").records, Some(0));
        assert_eq!(class(&empty, "recordedEvents").oldest, None);
        for sequence in 1..=2 {
            events
                .insert_event(&trellis_events_runtime::ProjectedEvent {
                    stream_sequence: sequence,
                    event_id: None,
                    event_time: "2026-01-01T00:00:00Z".to_owned(),
                    subject: "events.v1.Test.Created".to_owned(),
                    owner_contract_id: None,
                    owner_event_name: None,
                    resolution: "malformed".to_owned(),
                    verification_status: "missing-proof".to_owned(),
                    publisher_kind: None,
                    publisher_deployment_id: None,
                    publisher_instance_id: None,
                    publisher_participant_id: None,
                    publisher_principal_id: None,
                    publisher_connection_id: None,
                    publisher_login_session_id: None,
                    authorization_context_digest: None,
                    trace_id: None,
                    traceparent: None,
                    payload_bytes: vec![b'p'; 1000],
                    headers_json: "{}".to_owned(),
                    payload_json: None,
                    payload_text: None,
                    decode_error: None,
                    projected_at: "2026-01-01T00:00:00Z".to_owned(),
                })
                .unwrap();
        }
        let classes = crate::health::storage::collect(&config, &nats).await;
        let recorded = class(&classes, "recordedEvents");
        assert_eq!(recorded.records, Some(2));
        assert!(recorded.bytes >= 2000);
        assert_eq!(
            recorded.oldest.map(|at| at.unix_timestamp()),
            Some(1_767_225_600)
        );
        assert!(class(&classes, "eventsDatabase").bytes >= recorded.bytes);
        assert_eq!(class(&classes, "jobsDatabase").records, Some(0));
        let jobs_stream = class(&classes, "jobHistory");
        assert_eq!(jobs_stream.records, Some(1));
        assert!(jobs_stream.bytes > 0 && jobs_stream.oldest.is_some());
        assert_eq!(jobs_stream.retention, "4 days");
        assert!(
            !classes.iter().any(|class| class.name == "authDatabase"),
            "a database not on this host is not reported as empty"
        );

        // Converging again with the same policy is a no-op that keeps data.
        resources(policy)
            .converge_streams(nats, &crate::capacity::Shortages::default())
            .await
            .unwrap();
        assert_eq!(
            info(&jetstream, HEALTH_STREAM).await.state.messages,
            health.state.messages
        );
    }
}
