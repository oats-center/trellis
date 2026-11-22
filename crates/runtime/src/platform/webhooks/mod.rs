//! Webhook intake: the endpoints services declare, served on the Trellis
//! origin at `/webhooks/<service name>/<random id>`.
//!
//! Intake runs the cheap checks and the credential verifiers (`token`,
//! `basic`, `address`) as a request arrives and removes the credential before
//! anything is written. An `async` endpoint stores the request in its
//! deployment's intake stream, answers, and a worker verifies it afterwards;
//! a `sync` endpoint verifies first and answers once the outcome is recorded.
//! A request that passes becomes a `trellis.webhooks@v1` `Received` event,
//! delivered through the endpoint's own durable consumer.

mod api;
mod intake;
mod limits;
mod process;
mod records;
mod request;
mod verify;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use base64::Engine as _;
use tokio::sync::watch;

use super::auth::webhooks::WebhookEndpointRecord;
use super::auth::OAuthStorage;
use crate::supervisor::RuntimeError;

pub(crate) use api::register as register_api;

/// The subject a service's decide handler for `endpoint` answers on.
pub(crate) fn decide_subject(deployment: &str, endpoint: &str) -> String {
    format!(
        "webhooks.v1.decide.{}.{}",
        trellis_protocol::encode_subject_token(deployment),
        trellis_protocol::encode_subject_token(endpoint)
    )
}

/// The dedupe KV bucket shared by every endpoint.
const DEDUPE_BUCKET: &str = "trellis_webhook_dedupe";
/// The longest dedupe window a deployment may choose, which is also how long
/// the bucket keeps an ID at most.
const DEDUPE_BUCKET_AGE: Duration = Duration::from_secs(30 * 86_400);

pub(crate) fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

pub(crate) fn rfc3339(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|time| {
            time.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

/// An endpoint's runtime settings: the deployment's value, else the IDL
/// member, else the Trellis default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) requests_per_second: u64,
    pub(crate) burst: u64,
    pub(crate) failures_per_minute: u64,
    pub(crate) max_body: u64,
    pub(crate) dedupe_window_ms: u64,
    pub(crate) sync_decide_deadline_ms: u64,
    pub(crate) async_decide_deadline_ms: u64,
    pub(crate) rotation_overlap_ms: u64,
    pub(crate) record_retention_ms: u64,
    pub(crate) intake_retention_ms: u64,
    pub(crate) intake_max_bytes: u64,
}

impl Settings {
    fn resolve(record: &WebhookEndpointRecord) -> Self {
        let set = |name: &str| record.settings.get(name).copied();
        let definition = &record.definition;
        let decide = definition
            .decide
            .as_ref()
            .and_then(|decide| decide.deadline_ms);
        let sync = definition.mode == trellis_idl::WebhookMode::Sync;
        Self {
            requests_per_second: set("requestsPerSecond").unwrap_or(50),
            burst: set("burst").unwrap_or(200),
            failures_per_minute: set("failuresPerMinute").unwrap_or(20),
            max_body: set("maxBody").or(definition.max_body).unwrap_or(256 << 10),
            dedupe_window_ms: set("dedupeWindow")
                .or(definition.dedupe_window_ms)
                .unwrap_or(72 * 3_600_000),
            sync_decide_deadline_ms: set("syncDecideDeadline")
                .or(decide.filter(|_| sync))
                .unwrap_or(2_000)
                .min(10_000),
            async_decide_deadline_ms: set("asyncDecideDeadline")
                .or(decide.filter(|_| !sync))
                .unwrap_or(30_000)
                .min(60_000),
            rotation_overlap_ms: set("rotationOverlap")
                .or(definition.rotation_overlap_ms)
                .unwrap_or(24 * 3_600_000),
            record_retention_ms: set("recordRetention")
                .unwrap_or(records::Retention::default().records_ms),
            intake_retention_ms: set("intakeRetention")
                .unwrap_or(records::Retention::default().intake_ms),
            intake_max_bytes: set("intakeMaxBytes").unwrap_or(1 << 30),
        }
    }

    pub(crate) fn decide_deadline_ms(&self, mode: trellis_idl::WebhookMode) -> u64 {
        match mode {
            trellis_idl::WebhookMode::Sync => self.sync_decide_deadline_ms,
            trellis_idl::WebhookMode::Async => self.async_decide_deadline_ms,
        }
    }
}

/// One endpoint as intake serves it.
#[derive(Debug)]
pub(crate) struct Endpoint {
    pub(crate) record: WebhookEndpointRecord,
    pub(crate) settings: Settings,
    /// Digest of the verifier policy: the verifiers and the plain settings
    /// they read. A queued request keeps its intake checks only while this
    /// is unchanged.
    pub(crate) policy: String,
}

impl Endpoint {
    fn new(record: WebhookEndpointRecord) -> Self {
        use sha2::Digest as _;
        let mut read = BTreeMap::new();
        for verifier in &record.definition.verifiers {
            if let trellis_idl::WebhookVerifier::Address { ranges: field }
            | trellis_idl::WebhookVerifier::Basic {
                username: field, ..
            } = verifier
            {
                read.insert(
                    field.clone(),
                    record
                        .values
                        .get(field)
                        .and_then(|value| value.current.clone()),
                );
            }
        }
        let policy = serde_json::to_vec(&serde_json::json!({
            "verifiers": record.definition.verifiers,
            "settings": read,
        }))
        .unwrap_or_default();
        let policy = sha2::Sha256::digest(policy)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            settings: Settings::resolve(&record),
            record,
            policy,
        }
    }

    /// A setting's current value as text, when set.
    pub(crate) fn text(&self, field: &str) -> Option<String> {
        self.record
            .values
            .get(field)
            .and_then(|value| value.current.as_ref())
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    /// When the value secret `field`'s newest change replaced stops being
    /// accepted, in milliseconds, while that value is still held.
    fn overlap_until(&self, field: &str) -> Option<i64> {
        let previous = self.record.values.get(field)?.previous.as_ref()?;
        Some(
            previous.changed_at.saturating_mul(1000).saturating_add(
                i64::try_from(self.settings.rotation_overlap_ms).unwrap_or(i64::MAX),
            ),
        )
    }

    /// The secrets a verifier may use at `now_ms`: the current one, and the
    /// one the newest change replaced while the overlap after that change
    /// lasts.
    pub(crate) fn secrets(&self, field: &str, now_ms: i64) -> verify::Secrets {
        verify::Secrets {
            current: self.text(field),
            previous: self
                .overlap_until(field)
                .filter(|until| now_ms < *until)
                .and_then(|_| self.record.values.get(field)?.previous.as_ref())
                .and_then(|previous| previous.value.as_str().map(str::to_owned)),
        }
    }

    /// When the previous secret stops being accepted, while it still is.
    pub(crate) fn previous_accepted_until(&self, field: &str) -> Option<i64> {
        self.overlap_until(field).filter(|until| now_ms() < *until)
    }

    /// The generation of secret `field` whose `key` value a match used.
    pub(crate) fn matched_generation(&self, field: &str, key: verify::KeyMatch) -> Option<i64> {
        let value = self.record.values.get(field)?;
        match key {
            verify::KeyMatch::Current => value.generation,
            verify::KeyMatch::Previous => {
                value.previous.as_ref().map(|previous| previous.generation)
            }
        }
    }

    /// The generation a match of secret `field` by a request that arrived
    /// while the field was at generation `captured` counts toward: the
    /// field's current generation, when the request arrived under it. A
    /// request that arrived before the newest change used the value it
    /// matched while that value was current, so it counts toward none.
    pub(crate) fn match_generation(&self, field: &str, captured: Option<i64>) -> Option<i64> {
        let generation = self.record.values.get(field)?.generation?;
        (captured == Some(generation)).then_some(generation)
    }

    /// Whether secret `field` still accepts the value of `generation` at
    /// `now_ms`: it is the current generation, or the one the newest change
    /// replaced while the overlap lasts. `None` is a value no change set,
    /// such as a default, accepted while the field has no generation.
    pub(crate) fn accepts(&self, field: &str, generation: Option<i64>, now_ms: i64) -> bool {
        let Some(value) = self.record.values.get(field) else {
            return false;
        };
        match generation {
            None => value.generation.is_none() && value.current.is_some(),
            Some(generation) => {
                value.generation == Some(generation)
                    || (value
                        .previous
                        .as_ref()
                        .is_some_and(|previous| previous.generation == generation)
                        && self
                            .overlap_until(field)
                            .is_some_and(|until| now_ms < until))
            }
        }
    }

    /// Whether `client` is within the CIDR ranges of setting `ranges`.
    pub(crate) fn allows(&self, ranges: &str, client: std::net::IpAddr) -> bool {
        self.record
            .values
            .get(ranges)
            .and_then(|value| value.current.as_ref())
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ranges| {
                ranges
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|range| {
                        range
                            .parse::<ipnet::IpNet>()
                            .map(|network| network.contains(&client))
                            .or_else(|_| range.parse::<std::net::IpAddr>().map(|ip| ip == client))
                            .unwrap_or(false)
                    })
            })
    }

    /// The generation each secret a verifier checks has now, for a request
    /// arriving now.
    pub(crate) fn generations(&self) -> BTreeMap<String, i64> {
        self.record
            .definition
            .verifiers
            .iter()
            .filter_map(|verifier| match verifier {
                trellis_idl::WebhookVerifier::Hmac(hmac) => Some(&hmac.key),
                trellis_idl::WebhookVerifier::Token { secret, .. } => Some(secret),
                trellis_idl::WebhookVerifier::Basic { password, .. } => Some(password),
                _ => None,
            })
            .filter_map(|field| {
                let generation = self.record.values.get(field)?.generation?;
                Some((field.clone(), generation))
            })
            .collect()
    }

    pub(crate) fn key(&self) -> (String, String) {
        (
            self.record.deployment_id.clone(),
            self.record.endpoint.clone(),
        )
    }
}

/// Every served endpoint, by URL ID and by deployment and name.
#[derive(Debug, Default)]
pub(crate) struct Directory {
    pub(crate) by_url: HashMap<String, Arc<Endpoint>>,
    pub(crate) by_name: BTreeMap<(String, String), Arc<Endpoint>>,
}

impl Directory {
    fn build(records: Vec<WebhookEndpointRecord>) -> Self {
        let mut directory = Self::default();
        for record in records {
            let endpoint = Arc::new(Endpoint::new(record));
            directory
                .by_url
                .insert(endpoint.record.current_url.clone(), Arc::clone(&endpoint));
            for url in &endpoint.record.previous_urls {
                directory.by_url.insert(url.clone(), Arc::clone(&endpoint));
            }
            directory.by_name.insert(endpoint.key(), endpoint);
        }
        directory
    }

    /// Deployments with endpoints, with the endpoint whose settings size the
    /// deployment's intake stream.
    fn deployments(&self) -> BTreeMap<String, Arc<Endpoint>> {
        let mut deployments = BTreeMap::new();
        for ((deployment, _), endpoint) in &self.by_name {
            deployments
                .entry(deployment.clone())
                .or_insert_with(|| Arc::clone(endpoint));
        }
        deployments
    }
}

/// Webhook intake shared by the HTTP route, the workers, and the API.
#[derive(Clone)]
pub(crate) struct Webhooks {
    inner: Arc<Inner>,
}

struct Inner {
    storage: OAuthStorage,
    directory: RwLock<Arc<Directory>>,
    records: records::Records,
    nats: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
    dedupe: async_nats::jetstream::kv::Store,
    publisher: watch::Receiver<Option<trellis_rs::service::EventPublisher>>,
    /// The configured Trellis origin, without a trailing slash.
    origin: String,
    /// The Console's own origin when it has one; webhook paths 404 there.
    console: Option<ConsoleHost>,
    /// The largest body that fits one event in a NATS message.
    ceiling: u64,
    limits: limits::Limits,
}

/// The intake stream of one deployment.
pub(crate) fn stream_name(deployment: &str) -> String {
    let token: String = deployment
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("TRELLIS_WEBHOOKS_{token}")
}

fn stream_subject(deployment: &str) -> String {
    format!(
        "trellis.webhooks.intake.{}",
        trellis_protocol::encode_subject_token(deployment)
    )
}

const WORKER: &str = "TRELLIS_WEBHOOKS_WORKER";

impl Webhooks {
    /// Open the records database and the dedupe bucket and load the endpoints.
    pub(crate) async fn open(
        storage: OAuthStorage,
        records_path: &Path,
        nats: async_nats::Client,
        publisher: watch::Receiver<Option<trellis_rs::service::EventPublisher>>,
        origin: &str,
        console_origin: &str,
    ) -> Result<Self, RuntimeError> {
        let platform = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
        let path = records_path.to_owned();
        let records = tokio::task::spawn_blocking(move || records::Records::open(&path))
            .await
            .map_err(|error| platform(&error))?
            .map_err(|error| platform(&error))?;
        let jetstream = async_nats::jetstream::new(nats.clone());
        let dedupe = match jetstream.get_key_value(DEDUPE_BUCKET).await {
            Ok(bucket) => bucket,
            Err(_) => jetstream
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: DEDUPE_BUCKET.into(),
                    history: 1,
                    max_age: DEDUPE_BUCKET_AGE,
                    storage: async_nats::jetstream::stream::StorageType::File,
                    ..Default::default()
                })
                .await
                .map_err(|error| platform(&error))?,
        };
        let max_payload = u64::try_from(nats.server_info().max_payload).unwrap_or(1 << 20);
        let origin = origin.trim_end_matches('/').to_owned();
        let console = ConsoleHost::new(&origin, console_origin);
        let webhooks = Self {
            inner: Arc::new(Inner {
                storage,
                directory: RwLock::new(Arc::new(Directory::default())),
                records,
                nats,
                jetstream,
                dedupe,
                publisher,
                origin,
                console,
                // The body travels base64-encoded inside the event, beside its
                // headers and envelope.
                ceiling: max_payload.saturating_sub(64 << 10) / 4 * 3,
                limits: limits::Limits::default(),
            }),
        };
        webhooks.reload().await?;
        Ok(webhooks)
    }

    /// The webhook routes on the Trellis listener.
    pub(crate) fn router(&self) -> axum::Router {
        intake::router(self.clone())
    }

    pub(crate) fn directory(&self) -> Arc<Directory> {
        self.inner
            .directory
            .read()
            .map(|directory| Arc::clone(&directory))
            .unwrap_or_default()
    }

    async fn reload(&self) -> Result<(), RuntimeError> {
        let storage = self.inner.storage.clone();
        let records =
            super::auth::blocking(move || super::auth::webhooks::load_endpoints(&storage)).await?;
        let directory = Arc::new(Directory::build(records));
        for (deployment, endpoint) in directory.deployments() {
            if let Err(error) = self.ensure_stream(&deployment, &endpoint.settings).await {
                tracing::warn!(%deployment, %error, "webhook intake stream unavailable");
            }
        }
        if let Ok(mut current) = self.inner.directory.write() {
            *current = directory;
        }
        Ok(())
    }

    /// Create the deployment's intake stream, or converge its limits in place.
    async fn ensure_stream(&self, deployment: &str, settings: &Settings) -> Result<(), String> {
        use async_nats::jetstream::stream;
        let config = stream::Config {
            name: stream_name(deployment),
            subjects: vec![format!("{}.>", stream_subject(deployment))],
            retention: stream::RetentionPolicy::Limits,
            // A full stream refuses new requests rather than push out requests
            // still waiting for verification.
            discard: stream::DiscardPolicy::New,
            max_age: Duration::from_millis(settings.intake_retention_ms),
            max_bytes: i64::try_from(settings.intake_max_bytes).unwrap_or(i64::MAX),
            storage: stream::StorageType::File,
            duplicate_window: Duration::from_secs(120),
            ..Default::default()
        };
        match self.inner.jetstream.get_stream(&config.name).await {
            Ok(mut existing) => {
                let info = existing.info().await.map_err(|error| error.to_string())?;
                if info.config.max_age != config.max_age
                    || info.config.max_bytes != config.max_bytes
                {
                    self.inner
                        .jetstream
                        .update_stream(config)
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
            Err(_) => {
                self.inner
                    .jetstream
                    .create_stream(config)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    /// Store a request in its deployment's intake stream, waiting for JetStream
    /// to confirm the write, and return its sequence.
    pub(crate) async fn store(&self, request: &request::StoredRequest) -> Result<u64, String> {
        let payload = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", request.attempt_id.as_str());
        let subject = format!(
            "{}.{}",
            stream_subject(&request.deployment_id),
            trellis_protocol::encode_subject_token(&request.endpoint)
        );
        let ack = tokio::time::timeout(Duration::from_secs(5), async {
            self.inner
                .jetstream
                .publish_with_headers(subject, headers, payload.into())
                .await
                .map_err(|error| error.to_string())?
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "intake stream write timed out".to_owned())??;
        Ok(ack.sequence)
    }

    /// Read a stored request back by its stream sequence.
    pub(crate) async fn stored(
        &self,
        deployment: &str,
        sequence: u64,
    ) -> Result<Option<request::StoredRequest>, String> {
        let stream = self
            .inner
            .jetstream
            .get_stream(stream_name(deployment))
            .await
            .map_err(|error| error.to_string())?;
        match stream.get_raw_message(sequence).await {
            Ok(message) => serde_json::from_slice(&message.payload)
                .map(Some)
                .map_err(|error| error.to_string()),
            Err(_) => Ok(None),
        }
    }

    /// Keep the directory current, verify stored `async` requests, and apply
    /// retention, until `stop` resolves.
    pub(crate) async fn run(
        self,
        mut changed: watch::Receiver<u64>,
        stop: crate::shutdown::StopHandle,
    ) -> Result<(), RuntimeError> {
        let pending = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let gauge = Arc::clone(&pending);
        let _pending = trellis_rs::telemetry::instruments::register_observable(
            trellis_rs::telemetry::instruments::ObservableFamily::WebhooksPending,
            Arc::new(move || {
                vec![(
                    gauge.load(std::sync::atomic::Ordering::Relaxed) as f64,
                    Vec::new(),
                )]
            }),
        );
        let mut workers: BTreeMap<String, tokio_util::task::AbortOnDropHandle<()>> =
            BTreeMap::new();
        let mut sweep = tokio::time::interval(Duration::from_secs(60));
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            for deployment in self.directory().deployments().into_keys() {
                if workers
                    .get(&deployment)
                    .is_none_or(|worker| worker.is_finished())
                {
                    let webhooks = self.clone();
                    let name = deployment.clone();
                    workers.insert(
                        deployment,
                        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                            webhooks.work(name).await;
                        })),
                    );
                }
            }
            tokio::select! {
                () = stop.stopped() => return Ok(()),
                result = changed.changed() => {
                    if result.is_err() {
                        return Ok(());
                    }
                    // Commits often come in bursts; take them as one reload.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    changed.mark_unchanged();
                    if let Err(error) = self.reload().await {
                        tracing::warn!(%error, "webhook endpoints could not be reloaded");
                    }
                }
                _ = sweep.tick() => {
                    self.sweep().await;
                    if let Ok(count) = self.inner.records.pending().await {
                        pending.store(count, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Expire requests that left the intake stream unverified and drop
    /// records past their retention, per deployment. Retention is the
    /// deployment's setting even once it has no endpoint left.
    async fn sweep(&self) {
        let storage = self.inner.storage.clone();
        let stored =
            super::auth::blocking(move || super::auth::webhooks::load_retention(&storage)).await;
        let stored = match stored {
            Ok(stored) => stored,
            Err(error) => {
                tracing::warn!(%error, "webhook record retention could not be read");
                return;
            }
        };
        let retention = stored
            .into_iter()
            .map(|(deployment, settings)| {
                let defaults = records::Retention::default();
                let retention = records::Retention {
                    intake_ms: settings
                        .get("intakeRetention")
                        .copied()
                        .unwrap_or(defaults.intake_ms),
                    records_ms: settings
                        .get("recordRetention")
                        .copied()
                        .unwrap_or(defaults.records_ms),
                };
                (deployment, retention)
            })
            .collect();
        if let Err(error) = self.inner.records.sweep(now_ms(), retention).await {
            tracing::warn!(%error, "webhook record retention failed");
        }
    }

    /// Verify the deployment's stored `async` requests as they arrive.
    async fn work(self, deployment: String) {
        use async_nats::jetstream::consumer;
        use futures_util::StreamExt as _;
        loop {
            let result = async {
                let stream = self
                    .inner
                    .jetstream
                    .get_stream(stream_name(&deployment))
                    .await
                    .map_err(|error| error.to_string())?;
                let consumer: consumer::PullConsumer = stream
                    .get_or_create_consumer(
                        WORKER,
                        consumer::pull::Config {
                            durable_name: Some(WORKER.into()),
                            ack_policy: consumer::AckPolicy::Explicit,
                            ack_wait: Duration::from_secs(120),
                            max_deliver: -1,
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let mut messages = consumer
                    .messages()
                    .await
                    .map_err(|error| error.to_string())?;
                while let Some(message) = messages.next().await {
                    let message = message.map_err(|error| error.to_string())?;
                    self.verify_stored(message).await;
                }
                Ok::<(), String>(())
            }
            .await;
            if !self.directory().deployments().contains_key(&deployment) {
                return;
            }
            if let Err(error) = result {
                tracing::warn!(%deployment, %error, "webhook worker restarting");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// The `Received` event's subject for one endpoint.
    pub(crate) fn received_subject(deployment: &str, endpoint: &str) -> String {
        let base = trellis_protocol::derive_event_subject("trellis.webhooks@v1", "Received")
            .unwrap_or_default();
        format!(
            "{base}.{}.{}",
            trellis_protocol::encode_subject_token(deployment),
            trellis_protocol::encode_subject_token(endpoint)
        )
    }
}

/// Where the Console has its own origin: its hostname, and its port when it
/// shares the Trellis hostname on another port.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConsoleHost {
    host: String,
    /// Set when only the port tells the two origins apart.
    port: Option<u16>,
    /// Whether a `Host` without a port is the Console's: true when the
    /// Console's port is its scheme's default and the Trellis port is not.
    portless: bool,
}

impl ConsoleHost {
    /// The Console's origin, when it is not the Trellis origin.
    pub(crate) fn new(trellis_origin: &str, console_origin: &str) -> Option<Self> {
        let trellis = url::Url::parse(trellis_origin).ok()?;
        let console = url::Url::parse(console_origin).ok()?;
        if console.origin() == trellis.origin() {
            return None;
        }
        let host = console.host_str()?.to_ascii_lowercase();
        if trellis.host_str().map(str::to_ascii_lowercase) != Some(host.clone()) {
            return Some(Self {
                host,
                port: None,
                portless: true,
            });
        }
        let port = console.port_or_known_default()?;
        // Origins that differ only by scheme share every port; nothing tells
        // a webhook request for the Console apart.
        if trellis.port_or_known_default() == Some(port) {
            return None;
        }
        Some(Self {
            host,
            port: Some(port),
            portless: console.port().is_none(),
        })
    }

    /// Whether a request's `Host` header names the Console's origin.
    pub(crate) fn names(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        let (name, port) = match host.rsplit_once(':') {
            Some((name, port)) if !port.contains(']') => (name, port.parse::<u16>().ok()),
            _ => (host.as_str(), None),
        };
        name == self.host
            && match (self.port, port) {
                (None, _) => true,
                (Some(console), Some(port)) => port == console,
                (Some(_), None) => self.portless,
            }
    }
}

/// The dedupe key stored for a delivery ID.
fn dedupe_entry(deployment: &str, endpoint: &str, id: &str) -> String {
    use sha2::Digest as _;
    format!(
        "{}.{}.{}",
        trellis_protocol::encode_subject_token(deployment),
        trellis_protocol::encode_subject_token(endpoint),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(id))
    )
}

#[cfg(test)]
mod tests {
    use super::ConsoleHost;

    fn refused(trellis: &str, console: &str, host: &str) -> bool {
        ConsoleHost::new(trellis, console).is_some_and(|console| console.names(host))
    }

    #[test]
    fn webhook_paths_are_refused_only_on_the_consoles_own_origin() {
        // The Console on the same hostname, another port: the Trellis port
        // serves webhooks, the Console's does not.
        let (trellis, console) = ("https://trellis.example:8443", "https://trellis.example");
        assert!(!refused(trellis, console, "trellis.example:8443"));
        assert!(!refused(trellis, console, "TRELLIS.example:8443"));
        assert!(refused(trellis, console, "trellis.example"));
        assert!(refused(trellis, console, "trellis.example:443"));
        let (trellis, console) = ("https://trellis.example", "https://trellis.example:9443");
        assert!(!refused(trellis, console, "trellis.example"));
        assert!(!refused(trellis, console, "trellis.example:443"));
        assert!(refused(trellis, console, "trellis.example:9443"));
        let (trellis, console) = ("https://[::1]:8443", "https://[::1]:9443");
        assert!(!refused(trellis, console, "[::1]:8443"));
        assert!(refused(trellis, console, "[::1]:9443"));
        // A Console with its own hostname refuses webhook paths on any port.
        let (trellis, console) = ("https://trellis.example", "https://console.example:8443");
        assert!(!refused(trellis, console, "trellis.example"));
        assert!(refused(trellis, console, "console.example"));
        assert!(refused(trellis, console, "console.example:8443"));
        // The Console at /console on the Trellis origin refuses nothing.
        assert!(!refused(trellis, trellis, "trellis.example"));
    }
}
