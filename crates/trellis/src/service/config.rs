//! Settings Trellis keeps for a service or device.
//!
//! A participant declares `config` fields in its contract. Administrators set
//! values through the CLI or Console; the participant reads them when it
//! connects and follows live changes. Restart-only fields keep the value read
//! at connect.
//!
//! A live change takes effect in two steps. Checks and derive builders run
//! against the candidate values first; if any refuses, the revision is
//! rejected, [`ConfigHandle::current`] keeps the values accepted last, and
//! [`ConfigHandle::changed`] yields nothing. Only an accepted revision changes
//! the values. Each revision's outcome is reported to Trellis, and a rejected
//! one degrades the `settings` health check until a later one is accepted.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::{FutureExt as _, StreamExt as _};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::client::{ServiceHealthCheck, TrellisClient, TrellisClientError};
use crate::generated::ConfigField;

use super::ServerError;

const AUTH_API: &str = "trellis.auth@v1";
/// Longest rejection reason Auth records, in characters.
const REASON_LIMIT: usize = 1024;

/// Revisions behind the values in effect.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigRevision {
    /// The deployment's revision, absent before its first write.
    pub deployment: Option<u32>,
    /// This instance's override revision, absent when it has none.
    pub instance: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConfigState {
    values: Arc<Map<String, Value>>,
    revision: ConfigRevision,
}

/// Why a check or derive builder refused candidate settings.
///
/// Any [`std::error::Error`] converts into one that refuses every field the
/// candidate changes, so `?` works inside checks and builders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rejection {
    fields: Option<Vec<String>>,
    reason: String,
}

impl Rejection {
    /// Refuse `fields`, saying why. Name each field as the generated
    /// `Config` struct does (`webhook_url`) or as declared (`webhookUrl`).
    #[must_use]
    pub fn new<I, S>(fields: I, reason: impl Into<String>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            fields: Some(fields.into_iter().map(Into::into).collect()),
            reason: reason.into(),
        }
    }

    /// Refuse every field the candidate changes, saying why.
    #[must_use]
    pub fn changed(reason: impl Into<String>) -> Self {
        Self {
            fields: None,
            reason: reason.into(),
        }
    }

    /// The fields refused, or `None` for every changed field.
    #[must_use]
    pub fn fields(&self) -> Option<&[String]> {
        self.fields.as_deref()
    }

    /// Why they were refused.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.fields {
            Some(fields) if !fields.is_empty() => {
                write!(formatter, "{}: {}", fields.join(", "), self.reason)
            }
            _ => formatter.write_str(&self.reason),
        }
    }
}

impl<E: std::error::Error> From<E> for Rejection {
    fn from(error: E) -> Self {
        Self::changed(error.to_string())
    }
}

/// What became of the revision read last.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Outcome {
    rejected: bool,
    revision: ConfigRevision,
    /// Declared names of the fields refused.
    fields: Vec<String>,
    reason: Option<String>,
}

impl Outcome {
    fn applied(revision: ConfigRevision) -> Self {
        Self {
            revision,
            ..Self::default()
        }
    }

    /// `awsKeyId, awsSecret: set both or neither`.
    fn describe(&self) -> String {
        let reason = self.reason.as_deref().unwrap_or("rejected");
        if self.fields.is_empty() {
            reason.to_owned()
        } else {
            format!("{}: {reason}", self.fields.join(", "))
        }
    }
}

type Check = Arc<dyn Fn(&Map<String, Value>) -> Result<(), Rejection> + Send + Sync>;
type Built = Box<dyn Any + Send + Sync>;
type Builder<T, D> = Arc<dyn Fn(T) -> BoxFuture<'static, Result<D, Rejection>> + Send + Sync>;

/// One `derive`'s value and how it is rebuilt, with its types erased.
trait Slot: Send + Sync {
    /// Declared names of the fields it is built from.
    fn fields(&self) -> &[&'static str];
    fn built(&self) -> bool;
    fn build(&self, values: Map<String, Value>) -> BoxFuture<'static, Result<Built, Rejection>>;
    /// Take a value built for an accepted revision; the replaced one drops here.
    fn install(&self, value: Built);
}

struct DeriveSlot<T, D> {
    fields: Vec<&'static str>,
    build: Builder<T, D>,
    value: Arc<RwLock<Option<Arc<D>>>>,
}

impl<T, D> Slot for DeriveSlot<T, D>
where
    T: DeserializeOwned + Send + 'static,
    D: Send + Sync + 'static,
{
    fn fields(&self) -> &[&'static str] {
        &self.fields
    }

    fn built(&self) -> bool {
        self.value.read().expect("derived value").is_some()
    }

    fn build(&self, values: Map<String, Value>) -> BoxFuture<'static, Result<Built, Rejection>> {
        let build = Arc::clone(&self.build);
        async move {
            let decoded = decode::<T>(&values).map_err(Rejection::from)?;
            let value = build(decoded).await?;
            Ok(Box::new(Arc::new(value)) as Built)
        }
        .boxed()
    }

    fn install(&self, value: Built) {
        if let Ok(value) = value.downcast::<Arc<D>>() {
            let replaced = self.value.write().expect("derived value").replace(*value);
            drop(replaced);
        }
    }
}

/// A value built from settings, rebuilt when the fields it reads change.
pub struct Derived<D> {
    value: Arc<RwLock<Option<Arc<D>>>>,
}

impl<D> Clone for Derived<D> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
        }
    }
}

impl<D> fmt::Debug for Derived<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Derived").finish_non_exhaustive()
    }
}

impl<D> Derived<D> {
    /// The value built from the settings in effect. Call it at each use: a
    /// value kept from an earlier call is the one built then.
    #[must_use]
    pub fn current(&self) -> Arc<D> {
        let value = self.value.read().expect("derived value");
        Arc::clone(
            value
                .as_ref()
                .expect("a derived value is built before derive returns it"),
        )
    }
}

/// Checks, derives, and what the participant reported last.
#[derive(Default)]
struct Registry {
    checks: Vec<Check>,
    derives: Vec<Arc<dyn Slot>>,
    /// A registration refused the values in effect and waits for a revision
    /// that passes, so every reading is checked, changed or not.
    blocked: bool,
    /// The revision read last, accepted or not.
    latest: ConfigRevision,
    config_url: Option<String>,
    /// The rejection printed last, so a repeat prints nothing.
    shown: Option<Outcome>,
    /// The outcome Auth confirmed last, so a repeat is not sent again.
    reported: Option<Outcome>,
}

/// Live settings shared by every [`ConfigHandle`] of one participant and the
/// task following their changes.
pub(crate) struct Live {
    fields: &'static [ConfigField],
    participant: String,
    /// Where outcomes are reported; absent for a participant without settings.
    client: Option<Arc<TrellisClient>>,
    status: Box<dyn Fn(&str) + Send + Sync>,
    accepted: watch::Sender<ConfigState>,
    outcome: watch::Sender<Outcome>,
    /// Revisions accepted so far; a waiting registration resolves once it grows.
    acceptances: watch::Sender<u64>,
    stopped: CancellationToken,
    registry: Mutex<Registry>,
}

impl Live {
    /// Settings that never change, for a participant that declares none.
    pub(crate) fn detached() -> Arc<Self> {
        let live = Self::new(
            &[],
            String::new(),
            None,
            Box::new(|_| {}),
            ConfigState::default(),
        );
        live.stopped.cancel();
        live
    }

    fn new(
        fields: &'static [ConfigField],
        participant: String,
        client: Option<Arc<TrellisClient>>,
        status: Box<dyn Fn(&str) + Send + Sync>,
        initial: ConfigState,
    ) -> Arc<Self> {
        let revision = initial.revision;
        Arc::new(Self {
            fields,
            participant,
            client,
            status,
            accepted: watch::channel(initial).0,
            outcome: watch::channel(Outcome::applied(revision)).0,
            acceptances: watch::channel(0).0,
            stopped: CancellationToken::new(),
            registry: Mutex::new(Registry {
                latest: revision,
                ..Registry::default()
            }),
        })
    }

    /// The declared name of `name`, given as declared or as the generated
    /// struct's snake_case field.
    fn declared(&self, name: &str) -> Option<&'static str> {
        let plain = |text: &str| text.replace('_', "").to_ascii_lowercase();
        let wanted = plain(name);
        self.fields
            .iter()
            .find(|field| field.name == name || plain(field.name) == wanted)
            .map(|field| field.name)
    }

    /// Run `checks` and then `derives` against `candidate`. A derive is
    /// rebuilt only when it has no value yet or one of its fields changed.
    /// Builders are skipped once a check refuses, so a refused revision
    /// builds nothing.
    async fn run(
        &self,
        candidate: &Map<String, Value>,
        changed: &[&'static str],
        checks: &[Check],
        derives: &[Arc<dyn Slot>],
    ) -> Result<Vec<(Arc<dyn Slot>, Built)>, Vec<(Vec<String>, String)>> {
        let named = |rejection: Rejection, otherwise: &[&'static str]| {
            let fields = match rejection.fields {
                Some(fields) => fields
                    .iter()
                    .filter_map(|name| self.declared(name))
                    .map(str::to_owned)
                    .collect(),
                None => otherwise.iter().map(|name| (*name).to_owned()).collect(),
            };
            (fields, rejection.reason)
        };
        let refused = checks
            .iter()
            .filter_map(|check| check(candidate).err())
            .map(|rejection| named(rejection, changed))
            .collect::<Vec<_>>();
        if !refused.is_empty() {
            return Err(refused);
        }
        let mut built = Vec::new();
        let mut refused = Vec::new();
        for slot in derives {
            if slot.built() && !slot.fields().iter().any(|name| changed.contains(name)) {
                continue;
            }
            match slot.build(candidate.clone()).await {
                Ok(value) => built.push((Arc::clone(slot), value)),
                Err(rejection) => refused.push(named(rejection, slot.fields())),
            }
        }
        // Values built for a refused revision drop here, never taking effect.
        if refused.is_empty() {
            Ok(built)
        } else {
            Err(refused)
        }
    }

    /// Check a newer reading and take it if it passes. Restart-only fields
    /// keep their connect-time value, and a field whose value was removed
    /// keeps its last one until the participant connects again.
    async fn take(&self, response: &Value) {
        let next = values_of(response);
        let revision = revision_of(response);
        let mut registry = self.registry.lock().await;
        registry.latest = revision;
        if let Some(url) = response["configUrl"].as_str() {
            registry.config_url = Some(url.to_owned());
        }
        let previous = Arc::clone(&self.accepted.borrow().values);
        let (candidate, changed) = merge(self.fields, &previous, &next);
        if changed.is_empty() && !registry.blocked {
            self.accepted.send_if_modified(|state| {
                state.revision = revision;
                false
            });
            self.settle(&mut registry, Outcome::applied(revision)).await;
            return;
        }
        let (checks, derives) = (registry.checks.clone(), registry.derives.clone());
        match self.run(&candidate, &changed, &checks, &derives).await {
            Err(refused) => {
                self.settle(&mut registry, rejected(revision, refused))
                    .await;
            }
            Ok(built) => {
                registry.blocked = false;
                // Only a change to a live field wakes `changed()`.
                let live = !changed.is_empty();
                self.accepted.send_if_modified(|state| {
                    state.revision = revision;
                    if live {
                        state.values = Arc::new(candidate);
                    }
                    live
                });
                for (slot, value) in built {
                    slot.install(value);
                }
                self.settle(&mut registry, Outcome::applied(revision)).await;
            }
        }
    }

    /// A new registration refused the values in effect: record it and return
    /// the acceptance count to wait past.
    async fn refuse_in_effect(
        &self,
        registry: &mut Registry,
        refused: Vec<(Vec<String>, String)>,
    ) -> u64 {
        registry.blocked = true;
        let revision = self.accepted.borrow().revision;
        // A newer revision already refused stays the one reported.
        if registry.latest == revision {
            self.settle(registry, rejected(revision, refused)).await;
        }
        *self.acceptances.borrow()
    }

    /// Wait until a revision is accepted after `after` acceptances.
    async fn wait_accepted(&self, after: u64) -> Result<(), ServerError> {
        let mut acceptances = self.acceptances.subscribe();
        tokio::select! {
            waited = acceptances.wait_for(|count| *count > after) => waited
                .map(|_| ())
                .map_err(|_| ServerError::Internal("the participant stopped".into())),
            () = self.stopped.cancelled() => {
                Err(ServerError::Internal("the participant stopped".into()))
            }
        }
    }

    /// Record what became of a reading, tell Auth, and say why on a refusal.
    async fn settle(&self, registry: &mut Registry, outcome: Outcome) {
        if !outcome.rejected {
            self.acceptances.send_modify(|count| *count += 1);
        }
        let changed = *self.outcome.borrow() != outcome;
        self.outcome.send_replace(outcome.clone());
        if let (true, Some(client)) = (changed, &self.client) {
            client.health().publish_soon();
        }
        if outcome.rejected && registry.shown.as_ref() != Some(&outcome) {
            registry.shown = Some(outcome.clone());
            let detail = outcome.describe();
            let next = if registry.blocked {
                (self.status)(&format!("Settings rejected: {detail}"));
                "It waits for settings that pass."
            } else {
                "It keeps the settings it accepted last."
            };
            tracing::warn!(
                participant = self.participant,
                fields = outcome.fields.join(", "),
                reason = outcome.reason,
                config_url = registry.config_url,
                "Settings rejected"
            );
            print_with_link(
                &format!(
                    "{} rejected its settings: {detail}\n{next}",
                    self.participant
                ),
                "Fix them",
                registry.config_url.as_deref(),
            );
        }
        let Some(client) = &self.client else { return };
        if registry.reported.as_ref() == Some(&outcome) {
            return;
        }
        match report(client, &outcome).await {
            Ok(_) => registry.reported = Some(outcome),
            // The next reading reports again.
            Err(error) => tracing::warn!(%error, "could not report applied settings"),
        }
    }
}

/// Live, typed settings for this participant.
///
/// `T` is the generated `Config` struct. Cloning shares the same live values,
/// checks, and derived values. Call [`Self::current`] at each use: a value
/// kept from an earlier call, or one cloned out of [`Self::changed`], keeps
/// the settings it was read from.
pub struct ConfigHandle<T> {
    live: Arc<Live>,
    state: watch::Receiver<ConfigState>,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ConfigHandle<T> {
    fn clone(&self) -> Self {
        Self {
            live: Arc::clone(&self.live),
            state: self.state.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for ConfigHandle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigHandle")
            .field("revision", &self.state.borrow().revision)
            .finish_non_exhaustive()
    }
}

impl<T: DeserializeOwned + Send + 'static> ConfigHandle<T> {
    pub(crate) fn new(live: &Arc<Live>) -> Self {
        Self {
            live: Arc::clone(live),
            state: live.accepted.subscribe(),
            marker: PhantomData,
        }
    }

    /// The values in effect: the revision accepted last.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if the stored values do not decode as
    /// `T`, which means the generated struct and the running build disagree.
    pub fn current(&self) -> Result<T, ServerError> {
        decode(&self.state.borrow().values)
    }

    /// The revisions behind [`Self::current`].
    #[must_use]
    pub fn revision(&self) -> ConfigRevision {
        self.state.borrow().revision
    }

    /// Wait until a revision that changes a field that applies live is
    /// accepted, then return the new values. A rejected revision never
    /// arrives here.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] once the participant stopped and no
    /// further change can arrive, or if the values do not decode as `T`.
    pub async fn changed(&mut self) -> Result<T, ServerError> {
        tokio::select! {
            changed = self.state.changed() => changed
                .map_err(|_| ServerError::Internal("the participant stopped".into()))?,
            () = self.live.stopped.cancelled() => {
                return Err(ServerError::Internal("the participant stopped".into()));
            }
        }
        let values = Arc::clone(&self.state.borrow_and_update().values);
        decode(&values)
    }

    /// Check every later revision before it takes effect, and the values in
    /// effect now. Returns once the values in effect pass.
    ///
    /// While they don't, it waits: the participant prints which fields were
    /// refused and why with a link to its settings, reports the rejection to
    /// Trellis, and returns when an administrator saves a revision that
    /// passes. A [`Rejection`] names the fields refused; any other error
    /// refuses every field the candidate changes.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if the participant stops while it
    /// waits.
    pub async fn check<F>(&self, check: F) -> Result<(), ServerError>
    where
        F: Fn(&T) -> Result<(), Rejection> + Send + Sync + 'static,
    {
        let check: Check = Arc::new(move |values: &Map<String, Value>| {
            check(&decode::<T>(values).map_err(Rejection::from)?)
        });
        let live = &self.live;
        let waiting = {
            let mut registry = live.registry.lock().await;
            registry.checks.push(Arc::clone(&check));
            let current = Arc::clone(&live.accepted.borrow().values);
            match live.run(&current, &[], &[check], &[]).await {
                Ok(_) => None,
                Err(refused) => Some(live.refuse_in_effect(&mut registry, refused).await),
            }
        };
        match waiting {
            Some(after) => live.wait_accepted(after).await,
            None => Ok(()),
        }
    }

    /// Build a value from settings, such as a client that holds a key.
    ///
    /// It is built now from the values in effect, and rebuilt during the
    /// check step of a revision that changes one of `fields`; a revision that
    /// changes only other fields keeps it. A builder that fails rejects that
    /// revision for `fields` and keeps the old value. A replaced value is
    /// dropped once its replacement takes effect, when no caller holds it.
    /// Returns once a value is built, waiting as [`Self::check`] does while
    /// it can't be.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if `fields` names a setting the
    /// participant doesn't declare, or if the participant stops while it
    /// waits.
    pub async fn derive<D, F, I, S>(&self, fields: I, build: F) -> Result<Derived<D>, ServerError>
    where
        D: Send + Sync + 'static,
        F: Fn(&T) -> Result<D, Rejection> + Send + Sync + 'static,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.derive_async(fields, move |values: T| std::future::ready(build(&values)))
            .await
    }

    /// [`Self::derive`] with a builder that awaits, such as one that opens a
    /// connection. It receives its own copy of the candidate values.
    ///
    /// # Errors
    ///
    /// As [`Self::derive`].
    pub async fn derive_async<D, F, Fut, I, S>(
        &self,
        fields: I,
        build: F,
    ) -> Result<Derived<D>, ServerError>
    where
        D: Send + Sync + 'static,
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<D, Rejection>> + Send + 'static,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let live = &self.live;
        let fields = fields
            .into_iter()
            .map(|name| {
                live.declared(name.as_ref()).ok_or_else(|| {
                    ServerError::Internal(format!(
                        "{} declares no setting named {}",
                        live.participant,
                        name.as_ref()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let value = Arc::new(RwLock::new(None));
        let builder: Builder<T, D> = Arc::new(move |values| build(values).boxed());
        let slot: Arc<dyn Slot> = Arc::new(DeriveSlot {
            fields,
            build: builder,
            value: Arc::clone(&value),
        });
        let waiting = {
            let mut registry = live.registry.lock().await;
            registry.derives.push(Arc::clone(&slot));
            let current = Arc::clone(&live.accepted.borrow().values);
            match live.run(&current, &[], &[], &[slot]).await {
                Ok(built) => {
                    for (slot, value) in built {
                        slot.install(value);
                    }
                    None
                }
                Err(refused) => Some(live.refuse_in_effect(&mut registry, refused).await),
            }
        };
        if let Some(after) = waiting {
            live.wait_accepted(after).await?;
        }
        Ok(Derived { value })
    }
}

fn decode<T: DeserializeOwned>(values: &Map<String, Value>) -> Result<T, ServerError> {
    serde_json::from_value(Value::Object(values.clone()))
        .map_err(|error| ServerError::Internal(format!("settings do not decode: {error}")))
}

/// An outcome refusing `revision` for every field `refused` names.
fn rejected(revision: ConfigRevision, refused: Vec<(Vec<String>, String)>) -> Outcome {
    let mut fields = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    for (names, reason) in refused {
        for name in names {
            if !fields.contains(&name) {
                fields.push(name);
            }
        }
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    Outcome {
        rejected: true,
        revision,
        fields,
        reason: Some(reasons.join("; ").chars().take(REASON_LIMIT).collect()),
    }
}

/// The live values and the task following their changes. Dropping it stops
/// the task.
pub(crate) struct LiveConfig {
    pub(crate) live: Arc<Live>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LiveConfig {
    fn drop(&mut self) {
        self.task.abort();
        self.live.stopped.cancel();
    }
}

fn action(name: &str) -> trellis_protocol::CatalogActionIdentity {
    trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: name.into(),
        direction: trellis_protocol::PermissionAction::Call,
    }
}

async fn read(client: &TrellisClient, at_connect: bool) -> Result<Value, TrellisClientError> {
    client
        .request_json_value(
            AUTH_API,
            action("Config.Own.Get"),
            &trellis_protocol::derive_rpc_subject(AUTH_API, "Config.Own.Get")?,
            &serde_json::json!({ "atConnect": at_connect }),
        )
        .await
}

/// Tell Auth what became of the revision read last.
async fn report(client: &TrellisClient, outcome: &Outcome) -> Result<Value, TrellisClientError> {
    let mut input = serde_json::json!({
        "state": if outcome.rejected { "rejected" } else { "applied" },
        "fields": outcome.fields,
    });
    if let Some(revision) = outcome.revision.deployment {
        input["revision"] = revision.into();
    }
    if let Some(revision) = outcome.revision.instance {
        input["instanceRevision"] = revision.into();
    }
    if let Some(reason) = &outcome.reason {
        input["reason"] = reason.as_str().into();
    }
    client
        .request_json_value(
            AUTH_API,
            action("Config.Own.Report"),
            &trellis_protocol::derive_rpc_subject(AUTH_API, "Config.Own.Report")?,
            &input,
        )
        .await
}

fn revision_of(response: &Value) -> ConfigRevision {
    let number = |key: &str| {
        response[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
    };
    ConfigRevision {
        deployment: number("deploymentRevision"),
        instance: number("instanceRevision"),
    }
}

fn values_of(response: &Value) -> Map<String, Value> {
    response["values"].as_object().cloned().unwrap_or_default()
}

/// Print `text`, then where to act on it, with the link as a QR code.
fn print_with_link(text: &str, verb: &str, url: Option<&str>) {
    match url {
        Some(url) => {
            let qr = qrcode::QrCode::new(url.as_bytes())
                .map(|code| {
                    code.render::<qrcode::render::unicode::Dense1x2>()
                        .quiet_zone(true)
                        .build()
                })
                .unwrap_or_default();
            eprintln!("\n{text}\n{verb} at {url}\nor scan:\n{qr}");
        }
        None => eprintln!("\n{text}"),
    }
}

/// Read the settings at connect. While a required value is missing, print
/// where an administrator sets it and wait; never start with a partial
/// configuration.
pub(crate) async fn load(
    client: &TrellisClient,
    participant: &str,
    on_missing: &(dyn Fn(&str) + Sync),
) -> Result<ConfigState, TrellisClientError> {
    let mut backoff = crate::retry::Backoff::new(Duration::from_secs(1), Duration::from_secs(15));
    let mut shown = String::new();
    loop {
        let response = read(client, true).await?;
        let missing = response["missing"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if missing.is_empty() {
            return Ok(ConfigState {
                values: Arc::new(values_of(&response)),
                revision: revision_of(&response),
            });
        }
        // A saved value that doesn't fit this build counts as missing; say why.
        let reasons = response["missing"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| response["invalid"][name.as_str()?].as_str())
            .map(|reason| format!("\n  {reason}"))
            .collect::<String>();
        if shown != format!("{missing}{reasons}") {
            on_missing(&missing);
            let url = response["configUrl"].as_str();
            tracing::warn!(
                participant,
                missing,
                invalid = reasons.trim(),
                config_url = url,
                "Waiting for an administrator to set required settings"
            );
            print_with_link(
                &format!("{participant} needs these settings before it starts: {missing}{reasons}"),
                "Set them",
                url,
            );
            shown = format!("{missing}{reasons}");
        }
        tokio::time::sleep(backoff.failed()).await;
    }
}

/// The candidate values after a fresh read and the live fields it changes.
/// Restart-only fields keep their connect-time value, and a field whose value
/// was removed keeps its last one until the participant connects again.
fn merge(
    fields: &[ConfigField],
    previous: &Map<String, Value>,
    next: &Map<String, Value>,
) -> (Map<String, Value>, Vec<&'static str>) {
    let mut values = previous.clone();
    let mut changed = Vec::new();
    for field in fields.iter().filter(|field| !field.restart) {
        if let Some(value) = next.get(field.name) {
            if previous.get(field.name) != Some(value) {
                values.insert(field.name.to_owned(), value.clone());
                changed.push(field.name);
            }
        }
    }
    (values, changed)
}

/// Follow `Config.Changed` for this participant. An event only prompts a
/// fresh authenticated read, so its payload is never trusted for values. The
/// `settings` health check reports a rejected revision until one is accepted;
/// `status` hears what a waiting registration waits for.
pub(crate) fn follow(
    client: Arc<TrellisClient>,
    fields: &'static [ConfigField],
    participant: &str,
    initial: ConfigState,
    deployment: String,
    instance: String,
    status: Box<dyn Fn(&str) + Send + Sync>,
) -> LiveConfig {
    let live = Live::new(
        fields,
        participant.to_owned(),
        Some(Arc::clone(&client)),
        status,
        initial,
    );
    // Weak, so the check the client holds doesn't keep the settings alive.
    let checked: Weak<Live> = Arc::downgrade(&live);
    client.health().add("settings", move || {
        let outcome = checked.upgrade().map(|live| live.outcome.borrow().clone());
        async move {
            match outcome {
                Some(outcome) if outcome.rejected => ServiceHealthCheck::degraded(format!(
                    "Settings rejected: {}",
                    outcome.describe()
                )),
                _ => ServiceHealthCheck::ok(),
            }
        }
    });
    let following = Arc::clone(&live);
    let task = tokio::spawn(async move {
        let live = following;
        let subject = match trellis_protocol::derive_event_subject(AUTH_API, "Config.Changed") {
            Ok(subject) => subject,
            Err(error) => {
                tracing::error!(%error, "cannot derive the settings change subject");
                return;
            }
        };
        let refresh = || async {
            match read(&client, false).await {
                Ok(response) => live.take(&response).await,
                Err(error) => tracing::warn!(%error, "could not read changed settings"),
            }
        };
        loop {
            // Taken before acquiring, so a publication made while acquiring is
            // still seen as a change.
            let mut publication = client.subscribe_transport_publication();
            let deadline = Instant::now() + Duration::from_millis(client.timeout_ms());
            let lease = match client
                .acquire_transport(&[], std::slice::from_ref(&subject), deadline)
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    tracing::warn!(%error, "settings watch interrupted; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            let mut subscription = match lease.nats().subscribe(subject.clone()).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    tracing::warn!(%error, "settings watch interrupted; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if let Err(error) = lease.nats().flush().await {
                tracing::warn!(%error, "settings watch interrupted; retrying");
                continue;
            }
            // Changes made while no subscription existed are read now.
            refresh().await;
            loop {
                tokio::select! {
                    message = subscription.next() => {
                        let Some(message) = message else { break };
                        let Ok(event) = serde_json::from_slice::<Value>(&message.payload) else {
                            continue;
                        };
                        if event["deploymentId"].as_str() != Some(deployment.as_str()) {
                            continue;
                        }
                        if event["instanceId"]
                            .as_str()
                            .is_some_and(|target| target != instance)
                        {
                            continue;
                        }
                        refresh().await;
                    }
                    () = lease.wait_lost() => break,
                    // A successor's publication ends this subscription. The
                    // next pass subscribes on the successor and reads again,
                    // and dropping this lease lets the old generation drain.
                    changed = publication.changed() => {
                        if changed.is_err() {
                            // The connection's transport is gone.
                            return;
                        }
                        let published = *publication.borrow_and_update();
                        if published.is_some_and(|id| id != lease.generation_id()) {
                            break;
                        }
                    }
                }
            }
        }
    });
    LiveConfig { live, task }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELDS: &[ConfigField] = &[
        ConfigField {
            name: "level",
            required: false,
            secret: false,
            restart: false,
        },
        ConfigField {
            name: "port",
            required: true,
            secret: false,
            restart: true,
        },
    ];

    fn values(values: Value) -> Map<String, Value> {
        values.as_object().unwrap().clone()
    }

    #[test]
    fn live_fields_change_while_restart_and_removed_fields_keep_their_value() {
        let previous = values(serde_json::json!({"level": "info", "port": 80}));
        let (candidate, changed) = merge(
            FIELDS,
            &previous,
            &values(serde_json::json!({"level": "debug", "port": 81})),
        );
        assert_eq!(
            candidate,
            values(serde_json::json!({"level": "debug", "port": 80}))
        );
        assert_eq!(changed, ["level"]);

        // Only a restart-only field changed: nothing to tell the service.
        let (_, changed) = merge(
            FIELDS,
            &previous,
            &values(serde_json::json!({"level": "info", "port": 81})),
        );
        assert!(changed.is_empty());

        // A removed value is kept until the next connect waits for it.
        let (candidate, changed) =
            merge(FIELDS, &previous, &values(serde_json::json!({"port": 80})));
        assert_eq!(candidate, previous);
        assert!(changed.is_empty());
    }
}
