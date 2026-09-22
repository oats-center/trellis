use std::time::Duration;

use async_nats::jetstream::{self, stream};

use crate::health::{
    DEFAULT_TRANSPORT_MAX_BYTES, DEFAULT_TRANSPORT_RETENTION_HOURS, HEALTH_STREAM, HEALTH_SUBJECT,
};
use crate::supervisor::RuntimeError;
use crate::{RuntimeConfig, RuntimeMode, SubsystemName};
use trellis_events_runtime::{DLQ_STREAM, DLQ_SUBJECTS, REPLAY_STREAM, REPLAY_SUBJECTS};

pub(crate) const EVENT_STREAM: &str = "trellis";
pub(crate) const EVENT_STREAM_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub(crate) const JOBS_STREAM: &str = "JOBS";
pub(crate) const JOBS_WORK_STREAM: &str = "JOBS_WORK";
pub(crate) const JOBS_ADVISORIES_STREAM: &str = "JOBS_ADVISORIES";

#[derive(Clone, Debug)]
pub(crate) struct ExpectedRuntimeResources {
    subsystems: &'static [SubsystemName],
    streams: Vec<stream::Config>,
}

impl ExpectedRuntimeResources {
    pub(crate) fn for_mode(mode: RuntimeMode, config: &RuntimeConfig) -> Self {
        let subsystems = mode.subsystems();
        let mut streams = Vec::new();
        if subsystems.contains(&SubsystemName::Platform)
            || subsystems.contains(&SubsystemName::Events)
        {
            streams.push(event_stream_config());
        }
        if subsystems.contains(&SubsystemName::Events) {
            streams.extend(events_stream_configs());
        }
        if subsystems.contains(&SubsystemName::Jobs) {
            streams.extend(jobs_stream_configs());
            if !subsystems.contains(&SubsystemName::Events) {
                streams.push(advisory_stream_config());
            }
        }
        if subsystems.contains(&SubsystemName::Health) {
            streams.push(health_stream_config(config));
        }
        Self {
            subsystems,
            streams,
        }
    }

    pub(crate) fn requires(&self, subsystem: SubsystemName) -> bool {
        self.subsystems.contains(&subsystem)
    }

    pub(crate) fn streams(&self) -> &[stream::Config] {
        &self.streams
    }

    pub(crate) async fn converge_streams(
        &self,
        client: async_nats::Client,
    ) -> Result<(), RuntimeError> {
        let jetstream = jetstream::new(client);
        for expected in &self.streams {
            match jetstream.get_stream(&expected.name).await {
                Ok(mut stream) => {
                    let info = stream
                        .info()
                        .await
                        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
                    if let Some(update) =
                        stream_update(&info.config, expected).map_err(|reason| {
                            RuntimeError::IncompatibleResource {
                                name: expected.name.clone(),
                                reason,
                            }
                        })?
                    {
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
        || actual.storage != expected.storage
        || actual.discard != expected.discard
        || actual.sources != expected.sources
    {
        return Err("stream identity, subjects, retention, storage, or discard policy differs");
    }

    if matches!(
        expected.name.as_str(),
        JOBS_ADVISORIES_STREAM | REPLAY_STREAM | DLQ_STREAM
    ) && actual.discard_new_per_subject != expected.discard_new_per_subject
    {
        return Err("per-subject discard policy differs");
    }

    let mut update = actual.clone();
    let mut changed = false;
    let limits = match expected.name.as_str() {
        EVENT_STREAM => &[Limit::Age][..],
        HEALTH_STREAM => &[Limit::Age, Limit::Bytes][..],
        JOBS_STREAM | JOBS_WORK_STREAM | JOBS_ADVISORIES_STREAM | REPLAY_STREAM | DLQ_STREAM => &[
            Limit::Age,
            Limit::Messages,
            Limit::MessagesPerSubject,
            Limit::Bytes,
        ][..],
        _ => return Err("stream is not owned by the runtime"),
    };
    for limit in limits {
        changed |= limit.expand(&mut update, expected)?;
    }

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

#[derive(Clone, Copy)]
enum Limit {
    Age,
    Messages,
    MessagesPerSubject,
    Bytes,
}

impl Limit {
    fn expand(
        self,
        actual: &mut stream::Config,
        expected: &stream::Config,
    ) -> Result<bool, &'static str> {
        let (current, target) = match self {
            Self::Age => {
                if actual.max_age == expected.max_age {
                    return Ok(false);
                }
                if expected.max_age.is_zero()
                    || (!actual.max_age.is_zero() && expected.max_age > actual.max_age)
                {
                    actual.max_age = expected.max_age;
                    return Ok(true);
                }
                return Err("maximum age would reduce retained data");
            }
            Self::Messages => (&mut actual.max_messages, expected.max_messages),
            Self::MessagesPerSubject => (
                &mut actual.max_messages_per_subject,
                expected.max_messages_per_subject,
            ),
            Self::Bytes => (&mut actual.max_bytes, expected.max_bytes),
        };
        if *current == target {
            Ok(false)
        } else if target == -1 || (*current >= 0 && target > *current) {
            *current = target;
            Ok(true)
        } else {
            Err("stream limit would reduce retained data")
        }
    }
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

fn jobs_stream_configs() -> [stream::Config; 2] {
    [
        stream::Config {
            name: JOBS_STREAM.to_owned(),
            subjects: vec!["trellis.jobs.>".to_owned()],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::Old,
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

fn events_stream_configs() -> [stream::Config; 3] {
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

fn advisory_stream_config() -> stream::Config {
    stream::Config {
        name: JOBS_ADVISORIES_STREAM.to_owned(),
        subjects: vec!["$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>".to_owned()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::New,
        max_messages: -1,
        max_messages_per_subject: -1,
        max_bytes: -1,
        ..Default::default()
    }
}

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
        .map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX))
        .unwrap_or(DEFAULT_TRANSPORT_MAX_BYTES);
    stream::Config {
        name: HEALTH_STREAM.to_owned(),
        subjects: vec![HEALTH_SUBJECT.to_owned()],
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

    fn config() -> RuntimeConfig {
        RuntimeConfig::from_toml_str("").expect("empty optional config")
    }

    fn names(mode: RuntimeMode) -> Vec<String> {
        ExpectedRuntimeResources::for_mode(mode, &config())
            .streams()
            .iter()
            .map(|stream| stream.name.clone())
            .collect()
    }

    #[test]
    fn runtime_mode_derives_only_owned_streams() {
        assert_eq!(names(RuntimeMode::Platform), [EVENT_STREAM]);
        assert_eq!(
            names(RuntimeMode::Jobs),
            [JOBS_STREAM, JOBS_WORK_STREAM, JOBS_ADVISORIES_STREAM]
        );
        assert_eq!(names(RuntimeMode::Health), [HEALTH_STREAM]);
        assert_eq!(
            names(RuntimeMode::Events),
            [
                EVENT_STREAM,
                DLQ_STREAM,
                REPLAY_STREAM,
                JOBS_ADVISORIES_STREAM
            ]
        );
        assert_eq!(
            names(RuntimeMode::All),
            [
                EVENT_STREAM,
                DLQ_STREAM,
                REPLAY_STREAM,
                JOBS_ADVISORIES_STREAM,
                JOBS_STREAM,
                JOBS_WORK_STREAM,
                HEALTH_STREAM,
            ]
        );
    }

    #[test]
    fn all_mode_is_the_union_without_duplicate_event_streams() {
        let expected = ExpectedRuntimeResources::for_mode(RuntimeMode::All, &config());
        assert_eq!(
            expected
                .streams()
                .iter()
                .filter(|stream| stream.name == EVENT_STREAM)
                .count(),
            1
        );
        for subsystem in [
            SubsystemName::Platform,
            SubsystemName::Jobs,
            SubsystemName::Health,
            SubsystemName::Events,
        ] {
            assert!(expected.requires(subsystem));
        }
    }

    #[test]
    fn stream_compatibility_rejects_non_durable_policies() {
        let expected = event_stream_config();
        let mut actual = expected.clone();
        actual.storage = stream::StorageType::Memory;
        assert!(stream_update(&actual, &expected).is_err());

        actual = expected.clone();
        actual.discard = stream::DiscardPolicy::New;
        assert!(stream_update(&actual, &expected).is_err());
    }

    #[test]
    fn authoritative_streams_only_expand_limits() {
        let [expected, _, _] = events_stream_configs();
        assert!(stream_update(&expected, &expected)
            .expect("exact policy is compatible")
            .is_none());

        let mut finite = expected.clone();
        finite.max_messages = 10;
        let update = stream_update(&finite, &expected)
            .expect("expansion is compatible")
            .expect("expansion requires update");
        assert_eq!(update.max_messages, -1);

        let mut destructive = expected.clone();
        destructive.max_messages = -1;
        let mut finite_expected = expected;
        finite_expected.max_messages = 10;
        assert!(stream_update(&destructive, &finite_expected).is_err());

        let mut wrong_subject = finite_expected.clone();
        wrong_subject.subjects = vec!["other.>".to_owned()];
        assert!(stream_update(&wrong_subject, &finite_expected).is_err());
    }

    #[test]
    fn events_evidence_streams_do_not_evict_authoritative_history() {
        let [dlq, replay, advisory] = events_stream_configs();
        for stream in [&dlq, &replay, &advisory] {
            assert_eq!(stream.max_age, Duration::ZERO);
            assert_eq!(stream.max_messages, -1);
            assert_eq!(stream.max_bytes, -1);
            assert_eq!(stream.discard, stream::DiscardPolicy::New);
        }
        assert_eq!(dlq.max_messages_per_subject, -1);
        assert_eq!(replay.max_messages_per_subject, 1);
        assert!(replay.discard_new_per_subject);
    }
}
