//! JetStream capacity: how much of the broker's storage and memory Trellis
//! uses and reserves against its limits, and what JetStream refused to create
//! or write for lack of room.
//!
//! JetStream reserves a stream's `max_bytes` against the account and the
//! server when the stream is created, whether or not it holds anything, and
//! refuses a stream whose reservation does not fit. A stream without a size
//! bound reserves nothing. A full stream refuses writes once its own limits
//! are reached. Every Trellis component that creates or writes runtime
//! streams recognizes these refusals with [`exhaustion`], reports them
//! through [`Shortages`], and retries with backoff instead of failing or
//! spinning.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;

/// The share of a limit at or above which usage or reservations are reported
/// degraded, so operators see exhaustion coming.
pub(crate) const DEGRADED_FRACTION: f64 = 0.85;

/// How often a repeated refusal of the same resource is logged again.
const LOG_EVERY: Duration = Duration::from_secs(10 * 60);

pub(crate) use trellis_rs::client::{exhaustion_in, Exhaustion};

/// Recognizes a JetStream refusal for lack of room in an error or its sources.
///
/// JetStream errors carry their code in the text async-nats renders,
/// `<description> (code <status>, error code <code>)`, and the runtime often
/// keeps only that text, so this reads the rendered chain.
pub(crate) fn exhaustion(error: &(dyn std::error::Error + 'static)) -> Option<Exhaustion> {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    exhaustion_in(&text)
}

/// Bytes in use and reserved against a limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    /// Bytes stored.
    pub(crate) used: u64,
    /// Bytes the streams' size bounds reserve.
    pub(crate) reserved: u64,
    /// The limit, when one is set.
    pub(crate) limit: Option<u64>,
}

impl Usage {
    /// The larger of the used and reserved shares of the limit.
    fn fraction(&self) -> Option<f64> {
        let limit = self.limit.filter(|limit| *limit > 0)?;
        Some(self.used.max(self.reserved) as f64 / limit as f64)
    }

    /// `used`, `reserved`, and `limit` in words, as in
    /// "used 5.5 MiB, reserved 1.0 GiB of 1.0 GiB".
    pub(crate) fn describe(&self) -> String {
        let mut text = format!(
            "used {}, reserved {}",
            bytes(self.used),
            bytes(self.reserved)
        );
        match self.limit {
            Some(limit) => text.push_str(&format!(" of {}", bytes(limit))),
            None => text.push_str(", no limit"),
        }
        text
    }
}

/// One stream's share of the budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamUsage {
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) messages: u64,
    /// The stream's size bound, which JetStream reserves.
    pub(crate) max_bytes: Option<u64>,
}

/// JetStream usage and limits as Trellis sees them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Capacity {
    /// File storage of the Trellis account.
    pub(crate) storage: Usage,
    /// Memory storage of the Trellis account.
    pub(crate) memory: Usage,
    /// File storage of the whole server, when the system account is reachable.
    pub(crate) server_storage: Option<Usage>,
    /// Memory storage of the whole server, when the system account is reachable.
    pub(crate) server_memory: Option<Usage>,
    pub(crate) streams: u64,
    pub(crate) max_streams: Option<u64>,
    pub(crate) consumers: u64,
    pub(crate) max_consumers: Option<u64>,
    /// The account's streams, largest stored or reserved first.
    pub(crate) largest: Vec<StreamUsage>,
}

/// How many of the largest streams a capacity read lists.
const LARGEST: usize = 10;

impl Capacity {
    /// Reads the Trellis account's JetStream usage and limits, and the
    /// server's when `system` is a system-account connection.
    ///
    /// # Errors
    ///
    /// Returns the account-info failure text when JetStream does not answer.
    pub(crate) async fn read(
        trellis: &async_nats::Client,
        system: Option<&async_nats::Client>,
    ) -> Result<Self, String> {
        let jetstream = async_nats::jetstream::new(trellis.clone());
        let account = jetstream
            .query_account()
            .await
            .map_err(|error| error.to_string())?;
        let limit = |limit: Option<i64>| limit.and_then(|limit| u64::try_from(limit).ok());
        let mut largest = Vec::new();
        let mut streams = jetstream.streams();
        while let Some(Ok(info)) = streams.next().await {
            largest.push(StreamUsage {
                name: info.config.name.clone(),
                bytes: info.state.bytes,
                messages: info.state.messages,
                max_bytes: u64::try_from(info.config.max_bytes)
                    .ok()
                    .filter(|bytes| *bytes > 0),
            });
        }
        largest.sort_by_key(|stream| {
            std::cmp::Reverse(stream.bytes.max(stream.max_bytes.unwrap_or(0)))
        });
        largest.truncate(LARGEST);
        let server = match system {
            Some(system) => server_usage(system).await,
            None => None,
        };
        Ok(Self {
            storage: Usage {
                used: account.storage,
                reserved: account.reserved_storage,
                limit: limit(account.limits.max_storage),
            },
            memory: Usage {
                used: account.memory,
                reserved: account.reserved_memory,
                limit: limit(account.limits.max_memory),
            },
            server_storage: server.map(|(storage, _)| storage),
            server_memory: server.map(|(_, memory)| memory),
            streams: account.streams as u64,
            max_streams: limit(account.limits.max_streams),
            consumers: account.consumers as u64,
            max_consumers: limit(account.limits.max_consumers),
            largest,
        })
    }

    /// What is at or above [`DEGRADED_FRACTION`] of its limit, in words.
    pub(crate) fn pressure(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, usage) in [
            ("Trellis account storage", Some(&self.storage)),
            ("Trellis account memory", Some(&self.memory)),
            ("JetStream server storage", self.server_storage.as_ref()),
            ("JetStream server memory", self.server_memory.as_ref()),
        ] {
            let Some(usage) = usage else { continue };
            if let Some(fraction) = usage.fraction().filter(|f| *f >= DEGRADED_FRACTION) {
                problems.push(format!(
                    "{name} is {:.0}% committed: {}",
                    fraction * 100.0,
                    usage.describe()
                ));
            }
        }
        for (name, count, limit) in [
            ("streams", self.streams, self.max_streams),
            ("consumers", self.consumers, self.max_consumers),
        ] {
            if let Some(limit) = limit.filter(|limit| *limit > 0) {
                if count as f64 >= limit as f64 * DEGRADED_FRACTION {
                    problems.push(format!("{count} of {limit} JetStream {name} in use"));
                }
            }
        }
        problems
    }

    /// The numbers behind a refusal, for the log line that reports it.
    pub(crate) fn describe(&self) -> String {
        let mut text = format!("account storage {}", self.storage.describe());
        if let Some(server) = &self.server_storage {
            text.push_str(&format!("; server storage {}", server.describe()));
        }
        text.push_str(&format!("; {} streams", self.streams));
        if let Some(max) = self.max_streams {
            text.push_str(&format!(" of {max}"));
        }
        text
    }
}

/// The server's JetStream storage and memory, from the system account's
/// `JSZ` request. In a cluster each server answers; the most committed one
/// is the one that refuses first, so it is the one reported.
async fn server_usage(system: &async_nats::Client) -> Option<(Usage, Usage)> {
    #[derive(serde::Deserialize)]
    struct Reply {
        data: Option<Data>,
    }
    #[derive(serde::Deserialize)]
    struct Data {
        #[serde(default)]
        memory: u64,
        #[serde(default)]
        storage: u64,
        #[serde(default)]
        reserved_memory: u64,
        #[serde(default)]
        reserved_storage: u64,
        config: Option<Config>,
    }
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(default)]
        max_memory: i64,
        #[serde(default)]
        max_storage: i64,
    }
    let inbox = system.new_inbox();
    let mut replies = system.subscribe(inbox.clone()).await.ok()?;
    system
        .publish_with_reply("$SYS.REQ.SERVER.PING.JSZ", inbox, "{}".into())
        .await
        .ok()?;
    let mut best: Option<(Usage, Usage)> = None;
    let deadline = tokio::time::sleep(Duration::from_millis(500));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => break,
            reply = replies.next() => {
                let Some(reply) = reply else { break };
                let Some(data) = serde_json::from_slice::<Reply>(&reply.payload)
                    .ok()
                    .and_then(|reply| reply.data)
                else {
                    continue;
                };
                let config = data.config.unwrap_or(Config { max_memory: 0, max_storage: 0 });
                let server = (
                    Usage {
                        used: data.storage,
                        reserved: data.reserved_storage,
                        limit: u64::try_from(config.max_storage).ok().filter(|limit| *limit > 0),
                    },
                    Usage {
                        used: data.memory,
                        reserved: data.reserved_memory,
                        limit: u64::try_from(config.max_memory).ok().filter(|limit| *limit > 0),
                    },
                );
                let fraction = |usage: &(Usage, Usage)| usage.0.fraction().unwrap_or(0.0);
                if best.as_ref().is_none_or(|best| fraction(&server) > fraction(best)) {
                    best = Some(server);
                }
            }
        }
    }
    best
}

/// A byte count in binary units, as in `5.5 MiB`.
pub(crate) fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// One resource JetStream keeps refusing.
#[derive(Clone, Debug)]
struct Shortage {
    kind: Exhaustion,
    /// The refusal with its numbers, as [`Shortages::refused`] returned it.
    reason: String,
    logged: Instant,
}

/// The runtime resources JetStream currently refuses to create or write, and
/// the system-account connection capacity reads use when this process has
/// one.
///
/// A refusal is logged with the capacity numbers when first seen and at most
/// every ten minutes while it repeats, and `Health.Storage` reports every
/// outstanding one until the resource is created or written again.
#[derive(Debug, Default)]
pub(crate) struct Shortages {
    refused: Mutex<BTreeMap<String, Shortage>>,
    system: OnceLock<async_nats::Client>,
}

impl Shortages {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Lets capacity reads include the server's own numbers.
    pub(crate) fn set_system(&self, system: async_nats::Client) {
        let _ = self.system.set(system);
    }

    /// The system-account connection, when this process has one.
    pub(crate) fn system(&self) -> Option<&async_nats::Client> {
        self.system.get()
    }

    /// Records that JetStream refused `resource` for `exhaustion` and returns
    /// the refusal with the capacity numbers behind it. The refusal is logged
    /// when first seen, when its kind changes, and every ten minutes while it
    /// repeats. `wanted` is the size bound the resource asked for.
    pub(crate) async fn refused(
        &self,
        trellis: &async_nats::Client,
        resource: &str,
        exhaustion: Exhaustion,
        wanted: Option<u64>,
        error: &str,
    ) -> String {
        let numbers = match Capacity::read(trellis, self.system()).await {
            Ok(capacity) => capacity.describe(),
            Err(error) => format!("usage unavailable: {error}"),
        };
        let wanted = wanted.map_or_else(|| "no size bound".to_owned(), bytes);
        let reason = format!(
            "{}: {resource} asks for {wanted}; {numbers}",
            exhaustion.describe()
        );
        let now = Instant::now();
        let log = self.refused.lock().is_ok_and(|mut refused| {
            let previous = refused.get(resource);
            let log = previous.is_none_or(|shortage| {
                now.duration_since(shortage.logged) >= LOG_EVERY || shortage.kind != exhaustion
            });
            let logged = match previous {
                Some(shortage) if !log => shortage.logged,
                _ => now,
            };
            refused.insert(
                resource.to_owned(),
                Shortage {
                    kind: exhaustion,
                    reason: reason.clone(),
                    logged,
                },
            );
            log
        });
        if log {
            tracing::error!(
                %resource,
                %reason,
                %error,
                "JetStream refused a Trellis resource; retrying with backoff"
            );
        }
        reason
    }

    /// Clears a refusal once the resource was created or written.
    pub(crate) fn recovered(&self, resource: &str) {
        let removed = self
            .refused
            .lock()
            .ok()
            .and_then(|mut refused| refused.remove(resource));
        if removed.is_some() {
            tracing::info!(%resource, "JetStream accepted a Trellis resource it had refused");
        }
    }

    /// Every outstanding refusal, with its numbers.
    pub(crate) fn outstanding(&self) -> Vec<String> {
        self.refused
            .lock()
            .map(|refused| {
                refused
                    .values()
                    .map(|shortage| shortage.reason.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jetstream_refusals_are_recognized_from_their_rendered_errors() {
        for (text, expected) in [
            (
                "jetstream error: insufficient storage resources available \
                (code 500, error code 10047)",
                Some(Exhaustion::Storage),
            ),
            (
                "other error: maximum bytes exceeded (code 503, error code 10077)",
                Some(Exhaustion::Full),
            ),
            (
                "maximum messages per subject exceeded (code 503, error code 10077)",
                Some(Exhaustion::Full),
            ),
            (
                "maximum number of streams reached (code 400, error code 10027)",
                Some(Exhaustion::Streams),
            ),
            (
                "maximum consumers limit reached (code 400, error code 10026)",
                Some(Exhaustion::Consumers),
            ),
            (
                "insufficient memory resources available (code 500, error code 10028)",
                Some(Exhaustion::Memory),
            ),
            // A store failure that is not a limit is not a shortage.
            (
                "stream store failed: disk I/O error (code 503, error code 10077)",
                None,
            ),
            ("stream not found (code 404, error code 10059)", None),
        ] {
            assert_eq!(exhaustion_in(text), expected, "{text}");
        }
    }
}
