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
use std::time::Duration;

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
    /// File storage of the server with the most committed, when the system
    /// account is reachable.
    pub(crate) server_storage: Option<Usage>,
    /// Memory storage of the server with the most committed, when the system
    /// account is reachable.
    pub(crate) server_memory: Option<Usage>,
    pub(crate) streams: u64,
    pub(crate) max_streams: Option<u64>,
    pub(crate) consumers: u64,
    pub(crate) max_consumers: Option<u64>,
    /// The account's streams, largest stored or reserved first; `None` when
    /// JetStream did not list them all.
    pub(crate) largest: Option<Vec<StreamUsage>>,
}

/// The [`LARGEST`] streams of a listing by bytes stored or reserved, or why
/// the listing failed. A listing that fails partway yields nothing, so a
/// partial list never passes for the largest streams.
async fn largest_streams<E: std::fmt::Display>(
    mut streams: impl futures_util::Stream<Item = Result<async_nats::jetstream::stream::Info, E>>
        + Unpin,
) -> Result<Vec<StreamUsage>, String> {
    let mut largest = Vec::new();
    while let Some(info) = streams.next().await {
        let info = info.map_err(|error| error.to_string())?;
        largest.push(StreamUsage {
            name: info.config.name.clone(),
            bytes: info.state.bytes,
            messages: info.state.messages,
            max_bytes: u64::try_from(info.config.max_bytes)
                .ok()
                .filter(|bytes| *bytes > 0),
        });
    }
    largest
        .sort_by_key(|stream| std::cmp::Reverse(stream.bytes.max(stream.max_bytes.unwrap_or(0))));
    largest.truncate(LARGEST);
    Ok(largest)
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
        let largest = largest_streams(jetstream.streams())
            .await
            .inspect_err(|error| tracing::warn!(%error, "JetStream streams could not be listed"))
            .ok();
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
/// `JSZ` request. In a cluster each server answers; for storage and for memory
/// the most committed server is the one that refuses first, so it is the one
/// reported, and the two can be different servers.
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
                let storage = Usage {
                    used: data.storage,
                    reserved: data.reserved_storage,
                    limit: u64::try_from(config.max_storage).ok().filter(|limit| *limit > 0),
                };
                let memory = Usage {
                    used: data.memory,
                    reserved: data.reserved_memory,
                    limit: u64::try_from(config.max_memory).ok().filter(|limit| *limit > 0),
                };
                let fraction = |usage: &Usage| usage.fraction().unwrap_or(0.0);
                let most = |best: Usage, next: Usage| {
                    if fraction(&next) > fraction(&best) { next } else { best }
                };
                best = Some(match best {
                    None => (storage, memory),
                    Some((most_storage, most_memory)) => {
                        (most(most_storage, storage), most(most_memory, memory))
                    }
                });
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
    log: trellis_rs::__internal::LogLimit,
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
        let log = self.refused.lock().is_ok_and(|mut refused| {
            // A refusal of another kind is a new failure, logged at once.
            let mut log = refused
                .get(resource)
                .filter(|shortage| shortage.kind == exhaustion)
                .map_or_else(
                    || trellis_rs::__internal::LogLimit::new(LOG_EVERY),
                    |shortage| shortage.log,
                );
            let admitted = log.admit().is_some();
            refused.insert(
                resource.to_owned(),
                Shortage {
                    kind: exhaustion,
                    reason: reason.clone(),
                    log,
                },
            );
            admitted
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

    /// Drops a refusal of a resource Trellis no longer wants.
    pub(crate) fn forget(&self, resource: &str) {
        if let Ok(mut refused) = self.refused.lock() {
            refused.remove(resource);
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

    const MIB: i64 = 1 << 20;

    /// Starts the producer-resolved NATS executable on `config` and waits until
    /// it is ready.
    async fn start_nats(config: &std::path::Path) -> tokio::process::Child {
        use tokio::io::AsyncBufReadExt as _;
        let binary =
            std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
        let mut server = tokio::process::Command::new(binary)
            .arg("-c")
            .arg(config)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("producer-resolved NATS executable");
        let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(line) = output.next_line().await.unwrap() {
                if line.contains("Server is ready") {
                    return;
                }
            }
            panic!("NATS exited before readiness");
        })
        .await
        .expect("NATS readiness deadline");
        // Keep reading so the server never blocks on a full stderr pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = output.next_line().await {} });
        server
    }

    async fn connect(url: &str, user: &str) -> async_nats::Client {
        async_nats::ConnectOptions::with_user_and_password(user.into(), "pw".into())
            .request_timeout(Some(Duration::from_secs(2)))
            .connect(url)
            .await
            .unwrap()
    }

    // A cluster where one server's disk and another server's memory are
    // nearly committed. Each limit is reported from the server closest to
    // it, so the memory-heavy server is not hidden behind the disk-heavy one.
    #[tokio::test]
    async fn cluster_health_reports_the_most_committed_server_for_each_resource() {
        let directory = tempfile::tempdir().unwrap();
        let reservations = std::array::from_fn::<_, 4, _>(|_| {
            trellis_local_nats::ReservedPort::reserve().unwrap()
        });
        let [client_a, route_a, client_b, route_b] =
            reservations.each_ref().map(|reserved| reserved.port());
        let mut servers = Vec::new();
        for (name, tag, client, route, other) in [
            ("disk", "disk", client_a, route_a, route_b),
            ("memory", "memory", client_b, route_b, route_a),
        ] {
            let config = directory.path().join(format!("{name}.conf"));
            std::fs::write(
                &config,
                format!(
                    "server_name: {name}\nlisten: 127.0.0.1:{client}\nserver_tags: [{tag}]\n\
                    jetstream {{ store_dir: '{}', max_memory_store: 64MB, max_file_store: 64MB }}\n\
                    cluster {{ name: capacity, listen: 127.0.0.1:{route}, \
                    routes: [nats-route://127.0.0.1:{other}] }}\n\
                    accounts {{ SYS {{ users: [{{user: sys, password: pw}}] }}, \
                    APP {{ jetstream: enabled, users: [{{user: app, password: pw}}] }} }}\n\
                    system_account: SYS\n",
                    directory.path().join(name).display()
                ),
            )
            .unwrap();
            servers.push(start_nats(&config).await);
        }
        drop(reservations);
        let url = format!("nats://127.0.0.1:{client_a}");
        let jetstream = async_nats::jetstream::new(connect(&url, "app").await);
        let system = connect(&url, "sys").await;
        for (name, tag, storage) in [
            (
                "DISK",
                "disk",
                async_nats::jetstream::stream::StorageType::File,
            ),
            (
                "MEMORY",
                "memory",
                async_nats::jetstream::stream::StorageType::Memory,
            ),
        ] {
            // The cluster takes streams once it has a JetStream leader.
            tokio::time::timeout(Duration::from_secs(30), async {
                while jetstream
                    .create_stream(async_nats::jetstream::stream::Config {
                        name: name.into(),
                        subjects: vec![name.to_lowercase()],
                        storage,
                        max_bytes: 60 * MIB,
                        placement: Some(async_nats::jetstream::stream::Placement {
                            cluster: None,
                            tags: vec![tag.into()],
                        }),
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            })
            .await
            .expect("the cluster places the stream");
        }

        let committed = |usage: &Usage| usage.fraction().unwrap_or(0.0);
        let mut last = None;
        let reported = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                last = server_usage(&system).await;
                if let Some((storage, memory)) = last {
                    if committed(&storage) >= DEGRADED_FRACTION
                        && committed(&memory) >= DEGRADED_FRACTION
                    {
                        return (storage, memory);
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await;
        let (storage, memory) =
            reported.unwrap_or_else(|_| panic!("a nearly committed server is hidden: {last:?}"));
        assert_eq!(storage.reserved, 60 << 20);
        assert_eq!(memory.reserved, 60 << 20);
        let capacity = Capacity {
            server_storage: Some(storage),
            server_memory: Some(memory),
            ..Capacity::default()
        };
        let problems = capacity.pressure();
        assert!(
            problems
                .iter()
                .any(|problem| problem.starts_with("JetStream server storage is"))
                && problems
                    .iter()
                    .any(|problem| problem.starts_with("JetStream server memory is")),
            "{problems:?}"
        );
        drop(servers);
    }

    // A stream listing that fails, at once or after some streams arrived,
    // reports no largest streams rather than a short list, while the
    // account totals still come through.
    #[tokio::test]
    async fn a_failed_stream_listing_reports_no_largest_streams() {
        let directory = tempfile::tempdir().unwrap();
        let reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
        let port = reservation.port();
        let config = directory.path().join("nats.conf");
        std::fs::write(
            &config,
            format!(
                "listen: 127.0.0.1:{port}\njetstream {{ store_dir: '{}' }}\n\
                accounts {{ APP {{ jetstream: enabled, users: [\
                {{user: app, password: pw}}, \
                {{user: limited, password: pw, permissions: {{ publish: {{ \
                deny: [\"$JS.API.STREAM.LIST\"] }} }} }}] }} }}\n",
                directory.path().join("jetstream").display()
            ),
        )
        .unwrap();
        let _server = start_nats(&config).await;
        drop(reservation);
        let url = format!("nats://127.0.0.1:{port}");
        let app = connect(&url, "app").await;
        let limited = connect(&url, "limited").await;
        let jetstream = async_nats::jetstream::new(app.clone());
        for (name, max_bytes) in [("SMALL", MIB), ("LARGE", 8 * MIB)] {
            jetstream
                .create_stream(async_nats::jetstream::stream::Config {
                    name: name.into(),
                    subjects: vec![name.to_lowercase()],
                    max_bytes,
                    ..Default::default()
                })
                .await
                .unwrap();
        }

        let listed = Capacity::read(&app, None).await.unwrap();
        let names: Option<Vec<&str>> = listed
            .largest
            .as_ref()
            .map(|largest| largest.iter().map(|stream| stream.name.as_str()).collect());
        assert_eq!(names, Some(vec!["LARGE", "SMALL"]));

        let refused = Capacity::read(&limited, None).await.unwrap();
        assert_eq!(refused.streams, 2, "the account totals still come through");
        assert_eq!(refused.largest, None, "a refused listing is no list");

        // The first stream of a real listing, then the real refusal.
        let partway = async_nats::jetstream::new(app)
            .streams()
            .take(1)
            .chain(async_nats::jetstream::new(limited).streams());
        assert!(largest_streams(partway).await.is_err());
    }
}
