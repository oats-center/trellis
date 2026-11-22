//! Disk use and age of every data class the runtime keeps.
//!
//! The same read feeds `Health.Storage` and the `trellis.storage.*` gauges. It
//! only reads: stream info from JetStream and read-only SQLite queries.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use time::OffsetDateTime;

use crate::config::EVENT_STREAM_RETENTION_DAYS;
use crate::resources::{
    days, EVENT_STREAM, JOBS_ADVISORIES_STREAM, JOBS_STREAM, JOBS_STREAM_GRACE, JOBS_WORK_STREAM,
};
use crate::{RuntimeConfig, StorageBackend};
use trellis_events_runtime::{DLQ_STREAM, REPLAY_STREAM};

use super::{DEFAULT_HISTORY_RETENTION_DAYS, DEFAULT_TRANSPORT_RETENTION_HOURS, HEALTH_STREAM};

/// One measured data class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StorageClass {
    /// Wire token naming the data class.
    pub(crate) name: &'static str,
    /// Stream name or database file.
    pub(crate) location: String,
    /// Bytes on disk, or the estimated bytes of one table set.
    pub(crate) bytes: u64,
    /// Messages or rows, when countable.
    pub(crate) records: Option<u64>,
    /// When the oldest retained record was written or last changed.
    pub(crate) oldest: Option<OffsetDateTime>,
    /// The retention policy in a few words.
    pub(crate) retention: String,
}

/// Measure every data class this process can see.
///
/// A stream the selected mode does not create, or a database file not on
/// this host, is left out rather than reported as empty.
pub(crate) async fn collect(
    config: &RuntimeConfig,
    nats: &async_nats::Client,
) -> Vec<StorageClass> {
    let jetstream = async_nats::jetstream::new(nats.clone());
    let jobs_days = config.jobs_history_retention_days();
    let dead_letter_days = config.dead_letter_retention_days();
    let health_hours = config
        .health
        .as_ref()
        .and_then(|health| health.transport_retention_hours)
        .map_or(DEFAULT_TRANSPORT_RETENTION_HOURS, u64::from);
    let streams = [
        (
            EVENT_STREAM,
            "eventStream",
            format!("{EVENT_STREAM_RETENTION_DAYS} days"),
        ),
        (
            HEALTH_STREAM,
            "healthStream",
            format!("{health_hours} hours"),
        ),
        (
            JOBS_STREAM,
            "jobHistory",
            format!(
                "{} days",
                (days(jobs_days) + JOBS_STREAM_GRACE).as_secs() / 86_400
            ),
        ),
        (
            JOBS_WORK_STREAM,
            "jobWork",
            format!("until done; unclaimed expire after {jobs_days} days"),
        ),
        (
            JOBS_ADVISORIES_STREAM,
            "jobAdvisories",
            format!("{EVENT_STREAM_RETENTION_DAYS} days"),
        ),
        (
            DLQ_STREAM,
            "deadLetterJournal",
            format!("open kept; settled {dead_letter_days} days"),
        ),
        (
            REPLAY_STREAM,
            "replayStream",
            format!("{dead_letter_days} days"),
        ),
    ];
    let mut classes = Vec::new();
    for (stream_name, name, retention) in streams {
        let Ok(mut stream) = jetstream.get_stream(stream_name).await else {
            continue;
        };
        let Ok(info) = stream.info().await else {
            continue;
        };
        classes.push(StorageClass {
            name,
            location: stream_name.to_owned(),
            bytes: info.state.bytes,
            records: Some(info.state.messages),
            oldest: (info.state.messages > 0).then_some(info.state.first_timestamp),
            retention,
        });
    }

    let config = config.clone();
    if let Ok(databases) = tokio::task::spawn_blocking(move || databases(&config)).await {
        classes.extend(databases);
    }
    classes
}

fn databases(config: &RuntimeConfig) -> Vec<StorageClass> {
    let mut classes = Vec::new();
    let health_days = config
        .health
        .as_ref()
        .and_then(|health| health.history_retention_days)
        .map_or(DEFAULT_HISTORY_RETENTION_DAYS, i64::from);
    let events_days = config.events_history_retention_days();
    let dead_letter_days = config.dead_letter_retention_days();
    let jobs_days = config.jobs_history_retention_days();
    let paths = [
        (config.platform_storage_backend(), "authDatabase"),
        (config.health_storage_backend(), "healthDatabase"),
        (config.jobs_storage_backend(), "jobsDatabase"),
        (config.events_storage_backend(), "eventsDatabase"),
    ];
    for (backend, name) in paths {
        let Ok(StorageBackend::Sqlite(sqlite)) = backend else {
            continue;
        };
        let busy_timeout = sqlite.busy_timeout();
        let path = sqlite.path;
        let Some(bytes) = file_bytes(&path) else {
            continue;
        };
        let Ok(connection) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            continue;
        };
        if connection.busy_timeout(busy_timeout).is_err() {
            continue;
        }
        let location = path.display().to_string();
        let count_and_oldest = |sql: &str| {
            connection
                .query_row(sql, [], |row| {
                    Ok((row.get::<_, u64>(0)?, row.get::<_, Option<i64>>(1)?))
                })
                .optional()
                .ok()
                .flatten()
        };
        match name {
            "authDatabase" => classes.push(StorageClass {
                name,
                location,
                bytes,
                records: None,
                oldest: None,
                retention: "while in use; audit kept".to_owned(),
            }),
            "healthDatabase" => classes.push(StorageClass {
                name,
                location,
                bytes,
                records: None,
                oldest: connection
                    .query_row(
                        "SELECT MIN(bucket_start_ns) FROM health_metric_buckets",
                        [],
                        |row| row.get::<_, Option<i64>>(0),
                    )
                    .ok()
                    .flatten()
                    .and_then(from_nanos),
                retention: format!("{health_days} days"),
            }),
            "jobsDatabase" => {
                let counted =
                    count_and_oldest("SELECT COUNT(*), MIN(updated_at_nanos) FROM jobs_projection");
                classes.push(StorageClass {
                    name,
                    location,
                    bytes,
                    records: counted.map(|(count, _)| count),
                    oldest: counted.and_then(|(_, oldest)| oldest).and_then(from_nanos),
                    retention: format!("settled {jobs_days} days; failed and dead kept"),
                });
            }
            _ => {
                classes.push(StorageClass {
                    name,
                    location: location.clone(),
                    bytes,
                    records: None,
                    oldest: None,
                    retention: format!("{events_days} days"),
                });
                for (class, sql, retention) in [
                    (
                        "recordedEvents",
                        "SELECT COUNT(*), MIN(projected_at_ns), \
                        COALESCE(SUM(payload_size_bytes + length(headers_json) \
                        + COALESCE(length(payload_json), 0) \
                        + COALESCE(length(payload_text), 0)), 0) FROM events",
                        format!("{events_days} days"),
                    ),
                    (
                        "deadLetters",
                        "SELECT COUNT(*), MIN(updated_at_ns), \
                        COALESCE(SUM(length(payload_bytes) + length(headers_json)), 0) \
                        FROM consumer_dead_letters",
                        format!("open kept; settled {dead_letter_days} days"),
                    ),
                ] {
                    let Ok((records, oldest, estimate)) = connection.query_row(sql, [], |row| {
                        Ok((
                            row.get::<_, u64>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    }) else {
                        continue;
                    };
                    classes.push(StorageClass {
                        name: class,
                        location: location.clone(),
                        bytes: estimate.max(0) as u64,
                        records: Some(records),
                        oldest: oldest.and_then(from_nanos),
                        retention,
                    });
                }
            }
        }
    }
    classes
}

/// The database file plus its write-ahead log, which is disk the database
/// holds until the next checkpoint.
fn file_bytes(path: &Path) -> Option<u64> {
    let main = std::fs::metadata(path).ok()?.len();
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    Some(main + std::fs::metadata(wal).map_or(0, |metadata| metadata.len()))
}

fn from_nanos(nanos: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos)).ok()
}
