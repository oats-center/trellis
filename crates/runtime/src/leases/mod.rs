//! Runtime leases backed by NATS KV.

use std::{error::Error as StdError, fmt, time::Duration};

use async_nats::jetstream::{self, context, kv};
use bytes::Bytes;
use thiserror::Error;

use crate::ResolvedLeasesConfig;

/// Coordinates runtime leases in a NATS KV bucket.
///
/// A lease manager owns the runtime instance identity used as lease values and
/// exposes explicit acquire, renew, and release operations for callers that
/// need single-owner access to a runtime resource. Managers created with
/// [`LeaseManager::new`] hold only resolved settings; use [`LeaseManager::open`]
/// before performing lease operations against NATS.
#[derive(Clone)]
pub struct LeaseManager {
    /// NATS KV bucket that stores runtime lease keys.
    pub bucket: String,
    /// Runtime instance identifier written as the owner value for acquired leases.
    pub owner_id: String,
    /// Maximum lease age configured on the backing KV bucket.
    pub ttl: Duration,
    /// Recommended interval for renewing held leases.
    pub renew: Duration,
    store: Option<kv::Store>,
    jetstream: Option<jetstream::Context>,
    replicas: usize,
}

impl fmt::Debug for LeaseManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeaseManager")
            .field("bucket", &self.bucket)
            .field("owner_id", &self.owner_id)
            .field("ttl", &self.ttl)
            .field("renew", &self.renew)
            .field("replicas", &self.replicas)
            .field("store_open", &self.store.is_some())
            .finish()
    }
}

impl LeaseManager {
    /// Builds lease manager settings from resolved runtime config fields.
    #[must_use]
    pub fn new(config: &ResolvedLeasesConfig, owner_id: impl Into<String>) -> Self {
        Self {
            bucket: config.bucket.clone(),
            owner_id: owner_id.into(),
            ttl: Duration::from_millis(config.ttl_ms),
            renew: Duration::from_millis(config.renew_ms),
            store: None,
            jetstream: None,
            replicas: config.replicas.into(),
        }
    }

    /// Opens the NATS KV lease bucket and returns a manager backed by it.
    pub async fn open(
        jetstream: jetstream::Context,
        config: &ResolvedLeasesConfig,
        owner_id: impl Into<String>,
    ) -> Result<Self, LeaseError> {
        let mut manager = Self::new(config, owner_id);
        let store = open_or_create_store(
            &jetstream,
            manager.bucket.clone(),
            lease_bucket_config(&manager.bucket, manager.ttl, manager.replicas),
        )
        .await?;
        validate_store_config(&store, manager.ttl, manager.replicas).await?;
        manager.store = Some(store);
        manager.jetstream = Some(jetstream);
        Ok(manager)
    }

    /// Whether NATS has stopped expiring the lease bucket's records.
    ///
    /// Every record expires once it is older than the bucket's `max_age`, the
    /// lease lifetime, and a held lease is rewritten every renewal interval,
    /// so while NATS is healthy no record is older than the lifetime plus one
    /// interval. An older one means NATS stopped expiring the bucket, as it
    /// does after a critical storage write error such as a full disk, until
    /// NATS restarts. The age is taken from the stream's oldest record and
    /// the server's own clock, so clock skew between this host and NATS
    /// cannot make a healthy bucket look stalled.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Backend`] when NATS does not answer the stream
    /// info request.
    pub async fn stalled_expiry(&self) -> Result<Option<StalledExpiry>, LeaseError> {
        #[derive(serde::Deserialize)]
        struct Info {
            /// The server's clock when it answered.
            ts: Option<String>,
            state: State,
        }
        #[derive(serde::Deserialize)]
        struct State {
            messages: u64,
            first_ts: String,
        }
        let parse = |at: &str| {
            time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok()
        };
        let store = self.store()?;
        let backend = |message: String| LeaseError::Backend {
            key: None,
            operation: "inspect bucket",
            message,
        };
        let jetstream = self
            .jetstream
            .as_ref()
            .ok_or_else(|| backend("lease KV bucket is not open".to_owned()))?;
        let info = match jetstream
            .request::<_, _, jetstream::response::Response<Info>>(
                format!("STREAM.INFO.{}", store.stream_name),
                &serde_json::json!({}),
            )
            .await
        {
            Ok(jetstream::response::Response::Ok(info)) => info,
            Ok(jetstream::response::Response::Err { error }) => {
                return Err(backend(error.to_string()))
            }
            Err(error) => return Err(backend(error.to_string())),
        };
        let Some(oldest) = parse(&info.state.first_ts).filter(|_| info.state.messages > 0) else {
            return Ok(None);
        };
        // An older server does not send its clock; this host's stands in.
        let now = info
            .ts
            .as_deref()
            .and_then(parse)
            .unwrap_or_else(time::OffsetDateTime::now_utc);
        Ok(
            (now - oldest > self.ttl + self.renew).then(|| StalledExpiry {
                stream: store.stream_name.clone(),
                oldest,
                ttl: self.ttl,
            }),
        )
    }

    /// Attempts to acquire a runtime lease.
    pub async fn acquire(&self, key: LeaseKey) -> Result<LeaseGuard, LeaseError> {
        let store = self.store()?;
        match store
            .create(
                key.value.clone(),
                Bytes::copy_from_slice(self.owner_id.as_bytes()),
            )
            .await
        {
            Ok(revision) => Ok(LeaseGuard {
                key,
                fence: LeaseFence {
                    acquisition_revision: revision,
                },
                current_revision: revision,
            }),
            Err(error) if error.kind() == kv::CreateErrorKind::AlreadyExists => {
                Err(LeaseError::Held { key })
            }
            Err(error) => Err(LeaseError::Backend {
                key: Some(key),
                operation: "acquire",
                message: broker_failure(&error),
            }),
        }
    }

    /// Renews a held lease using the guard's current revision.
    ///
    /// This performs one compare-and-update operation against the backing KV
    /// entry. Callers that need continuous ownership should schedule renewals
    /// before [`LeaseManager::ttl`] expires and keep the guard returned by each
    /// successful renewal. If the update result is ambiguous, the current entry
    /// must still prove ownership before its revision is adopted.
    ///
    /// The result tells how the lease's lifetime is known. A written renewal
    /// started no earlier than this call. An adopted one may have been written
    /// by any earlier unacknowledged renewal of the same revision, so its
    /// lifetime runs from the first of those.
    pub async fn renew(&self, guard: &mut LeaseGuard) -> Result<Renewal, LeaseError> {
        let store = self.store()?;
        let key = guard.key.clone();
        match store
            .update(
                guard.key.value.clone(),
                Bytes::copy_from_slice(self.owner_id.as_bytes()),
                guard.current_revision,
            )
            .await
        {
            Ok(revision) => {
                guard.current_revision = revision;
                Ok(Renewal::Written)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    kv::UpdateErrorKind::TimedOut | kv::UpdateErrorKind::WrongLastRevision
                ) =>
            {
                match store.entry(key.value.clone()).await {
                    Ok(Some(entry))
                        if entry.operation == kv::Operation::Put
                            && entry.value.as_ref() == self.owner_id.as_bytes()
                            && entry.revision > guard.current_revision =>
                    {
                        guard.current_revision = entry.revision;
                        Ok(Renewal::Adopted)
                    }
                    Ok(Some(entry))
                        if entry.operation == kv::Operation::Put
                            && entry.value.as_ref() == self.owner_id.as_bytes() =>
                    {
                        Err(LeaseError::Backend {
                            key: Some(key),
                            operation: "renew",
                            message: format!(
                                "{} The lease record was not renewed.",
                                broker_failure(&error)
                            ),
                        })
                    }
                    Ok(Some(entry))
                        if matches!(
                            entry.operation,
                            kv::Operation::Delete | kv::Operation::Purge
                        ) =>
                    {
                        Err(LeaseError::NotHeld { key })
                    }
                    Ok(Some(_)) => Err(LeaseError::Stale { key }),
                    Ok(None) => Err(LeaseError::NotHeld { key }),
                    Err(inspect_error) => Err(LeaseError::Backend {
                        key: Some(key),
                        operation: "renew",
                        message: format!(
                            "{} Reading the lease record failed too: {inspect_error}",
                            broker_failure(&error)
                        ),
                    }),
                }
            }
            Err(error) => Err(classify_revision_error(store, key, "renew", error).await),
        }
    }

    /// Releases a held lease using the guard's current revision.
    pub async fn release(&self, guard: LeaseGuard) -> Result<(), LeaseError> {
        let store = self.store()?;
        let key = guard.key.clone();
        match store
            .delete_expect_revision(guard.key.value.clone(), Some(guard.current_revision))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(classify_revision_error(store, key, "release", error).await),
        }
    }

    fn store(&self) -> Result<&kv::Store, LeaseError> {
        self.store.as_ref().ok_or_else(|| LeaseError::Backend {
            key: None,
            operation: "open",
            message: "lease KV bucket is not open".to_owned(),
        })
    }
}

/// How a successful [`LeaseManager::renew`] advanced the lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Renewal {
    /// This call's own update was acknowledged.
    Written,
    /// The update's outcome was unknown, and the lease entry showed a newer
    /// revision of ours, written by this call or an earlier unacknowledged one.
    Adopted,
}

/// Proof that this runtime currently owns a lease key.
///
/// The guard carries a fixed acquisition fence and the last observed KV revision.
/// It must be passed back to [`LeaseManager::renew`] or [`LeaseManager::release`].
/// Revision checks make a stale guard fail instead of renewing or deleting
/// another owner's lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseGuard {
    key: LeaseKey,
    fence: LeaseFence,
    current_revision: u64,
}

impl LeaseGuard {
    /// Returns the acquired lease key.
    #[must_use]
    pub fn key(&self) -> &LeaseKey {
        &self.key
    }

    /// Returns the immutable acquisition fence for this ownership generation.
    #[must_use]
    pub(crate) fn fence(&self) -> LeaseFence {
        self.fence
    }

    /// Returns the current NATS KV revision used for renewal and release.
    #[must_use]
    pub fn current_revision(&self) -> u64 {
        self.current_revision
    }
}

/// Immutable fencing token for one lease ownership generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LeaseFence {
    acquisition_revision: u64,
}

impl LeaseFence {
    /// Returns the NATS KV revision assigned when ownership was acquired.
    #[must_use]
    pub(crate) fn acquisition_revision(self) -> u64 {
        self.acquisition_revision
    }
}

/// Canonical key for a runtime lease entry.
///
/// Lease keys are stored directly in the configured NATS KV bucket. Callers are
/// responsible for choosing stable, subsystem-scoped key strings that do not
/// collide with unrelated runtime resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseKey {
    /// Canonical lease key string.
    pub value: String,
}

impl LeaseKey {
    /// Creates a lease key.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// Returns the canonical lease key string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Lease operation error.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LeaseError {
    /// An existing lease bucket does not satisfy runtime lease invariants.
    #[error(
        "lease bucket {bucket:?} has incompatible {field}: expected {expected}, actual {actual}"
    )]
    InfrastructureMismatch {
        /// Lease bucket name.
        bucket: String,
        /// Incompatible bucket configuration field.
        field: &'static str,
        /// Required field value.
        expected: String,
        /// Actual field value reported by NATS.
        actual: String,
    },
    /// Lease is already held by another owner.
    #[error("lease {key:?} is already held")]
    Held {
        /// Lease key.
        key: LeaseKey,
    },
    /// Lease is not currently held.
    #[error("lease {key:?} is not held")]
    NotHeld {
        /// Lease key.
        key: LeaseKey,
    },
    /// Lease guard revision is stale.
    #[error("lease {key:?} has a stale revision")]
    Stale {
        /// Lease key.
        key: LeaseKey,
    },
    /// NATS KV backend operation failed.
    #[error("lease backend {operation} failed for {key:?}: {message}")]
    Backend {
        /// Lease key, when the failed operation targets a key.
        key: Option<LeaseKey>,
        /// Backend operation name.
        operation: &'static str,
        /// Backend error message.
        message: String,
    },
}

fn lease_bucket_config(bucket: &str, ttl: Duration, replicas: usize) -> kv::Config {
    kv::Config {
        bucket: bucket.to_owned(),
        history: 1,
        max_age: ttl,
        num_replicas: replicas,
        ..Default::default()
    }
}

async fn open_or_create_store(
    jetstream: &jetstream::Context,
    bucket: String,
    config: kv::Config,
) -> Result<kv::Store, LeaseError> {
    match jetstream.get_key_value(bucket.clone()).await {
        Ok(store) => Ok(store),
        Err(open_error) => match jetstream.create_key_value(config).await {
            Ok(store) => Ok(store),
            Err(create_error) if is_bucket_create_race(&create_error) => jetstream
                .get_key_value(bucket)
                .await
                .map_err(|retry_error| LeaseError::Backend {
                    key: None,
                    operation: "open",
                    message: format!(
                        "failed to open lease KV bucket after concurrent create: {retry_error}"
                    ),
                }),
            Err(create_error) => Err(LeaseError::Backend {
                key: None,
                operation: "open",
                message: format!(
                    "failed to open lease KV bucket ({open_error}) or create it ({create_error})"
                ),
            }),
        },
    }
}

async fn validate_store_config(
    store: &kv::Store,
    ttl: Duration,
    replicas: usize,
) -> Result<(), LeaseError> {
    let status = store.status().await.map_err(|error| LeaseError::Backend {
        key: None,
        operation: "inspect bucket",
        message: error.to_string(),
    })?;
    // Acquisition and renewal compare the latest revision, not retained history.
    // TTL and replica policy still determine lease expiry and durability.
    if status.max_age() != ttl {
        return Err(LeaseError::InfrastructureMismatch {
            bucket: store.name.clone(),
            field: "max_age",
            expected: format!("{}ms", ttl.as_millis()),
            actual: format!("{}ms", status.max_age().as_millis()),
        });
    }
    let actual_replicas = status.info.config.num_replicas;
    if replicas != 0 && actual_replicas != replicas {
        return Err(LeaseError::InfrastructureMismatch {
            bucket: store.name.clone(),
            field: "replicas",
            expected: replicas.to_string(),
            actual: actual_replicas.to_string(),
        });
    }
    Ok(())
}

async fn classify_revision_error(
    store: &kv::Store,
    key: LeaseKey,
    operation: &'static str,
    error: kv::UpdateError,
) -> LeaseError {
    if error.kind() != kv::UpdateErrorKind::WrongLastRevision {
        return LeaseError::Backend {
            key: Some(key),
            operation,
            message: broker_failure(&error),
        };
    }

    match store.entry(key.value.clone()).await {
        Ok(Some(entry)) => match entry.operation {
            kv::Operation::Delete | kv::Operation::Purge => LeaseError::NotHeld { key },
            _ => LeaseError::Stale { key },
        },
        Ok(None) => LeaseError::NotHeld { key },
        Err(inspect_error) => LeaseError::Backend {
            key: Some(key),
            operation,
            message: format!(
                "revision mismatch ({error}); failed to inspect current lease entry: \
                {inspect_error}"
            ),
        },
    }
}

/// A lease write NATS did not take, in an operator's words: the broker's own
/// refusal when JetStream answered with one, or that the bucket stopped
/// answering when the write timed out. A stream NATS has marked with a
/// critical storage write error, as after a full disk, answers no writes at
/// all, so a timeout points at the NATS log, which names the storage error.
fn broker_failure(error: &(dyn StdError + 'static)) -> String {
    let mut timed_out = false;
    let mut next = Some(error);
    while let Some(error) = next {
        if let Some(refusal) = error.downcast_ref::<jetstream::Error>() {
            return format!("NATS refused to store the lease: {refusal}.");
        }
        timed_out |= error
            .downcast_ref::<kv::UpdateError>()
            .is_some_and(|error| error.kind() == kv::UpdateErrorKind::TimedOut)
            || error
                .downcast_ref::<context::PublishError>()
                .is_some_and(|error| error.kind() == context::PublishErrorKind::TimedOut);
        next = error.source();
    }
    if timed_out {
        LEASE_BUCKET_SILENT.to_owned()
    } else {
        error.to_string()
    }
}

/// What a lease write that timed out tells an operator.
pub(crate) const LEASE_BUCKET_SILENT: &str = "The lease bucket stopped answering: NATS did not \
    confirm the write in time. Check the NATS log for storage errors, such as a full disk.";

/// Records in the lease bucket older than their lifetime: NATS has stopped
/// expiring them, and a lease such a record holds is never freed meanwhile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StalledExpiry {
    /// The bucket's JetStream stream, as `KV_trellis_runtime_leases`.
    pub stream: String,
    /// When the oldest record was written, by the NATS server's clock.
    pub oldest: time::OffsetDateTime,
    /// The records' lifetime, the bucket's `max_age`.
    pub ttl: Duration,
}

impl fmt::Display for StalledExpiry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let oldest = self
            .oldest
            .replace_nanosecond(0)
            .unwrap_or(self.oldest)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        write!(
            formatter,
            "NATS has stopped expiring the runtime lease records ({} holds a record from \
            {oldest}, older than its {}s lifetime). NATS usually does this after a storage write \
            error such as a full disk. Free disk space on the NATS host and restart NATS.",
            self.stream,
            self.ttl.as_secs()
        )
    }
}

fn is_bucket_create_race(error: &context::CreateKeyValueError) -> bool {
    if error.kind() != context::CreateKeyValueErrorKind::BucketCreate {
        return false;
    }

    has_stream_exists_error(error)
}

fn has_stream_exists_error(error: &dyn StdError) -> bool {
    let mut source = error.source();
    while let Some(error) = source {
        if let Some(stream_error) = error.downcast_ref::<context::CreateStreamError>() {
            if matches!(
                stream_error.kind(),
                context::CreateStreamErrorKind::JetStream(error)
                    if error.kind() == jetstream::ErrorCode::STREAM_NAME_EXIST
            ) {
                return true;
            }
        }
        if let Some(jetstream_error) = error.downcast_ref::<jetstream::Error>() {
            if jetstream_error.kind() == jetstream::ErrorCode::STREAM_NAME_EXIST {
                return true;
            }
        }
        source = error.source();
    }

    false
}

#[cfg(test)]
mod tests;
