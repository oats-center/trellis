use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use ref_cast::RefCast;
use rusqlite::{params, CachedStatement, OptionalExtension, Params, Row, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    canonicalize_json, AuthorityIssuerKey, AuthorityIssuerState, ProtocolError,
};

use super::revocation::{enqueue_scope, EnforcementScope};

/// Auth's view of its SQLite connection.
///
/// Every statement Auth runs is constant text, so `query_row`, `execute`, and
/// `prepare` go through the connection's prepared-statement cache instead of
/// parsing and planning the SQL again on each call. Everything else is the
/// ordinary rusqlite connection.
#[derive(RefCast)]
#[repr(transparent)]
pub(crate) struct Connection(rusqlite::Connection);

/// Covers the distinct statements of one busy path, such as admission, with room to spare.
const STATEMENT_CACHE_CAPACITY: usize = 256;

impl Connection {
    pub(crate) fn new(connection: &rusqlite::Connection) -> &Self {
        Self::ref_cast(connection)
    }

    pub(crate) fn query_row<T, P: Params, F: FnOnce(&Row<'_>) -> rusqlite::Result<T>>(
        &self,
        sql: &str,
        params: P,
        row: F,
    ) -> rusqlite::Result<T> {
        self.0.prepare_cached(sql)?.query_row(params, row)
    }

    pub(crate) fn execute<P: Params>(&self, sql: &str, params: P) -> rusqlite::Result<usize> {
        self.0.prepare_cached(sql)?.execute(params)
    }

    pub(crate) fn prepare(&self, sql: &str) -> rusqlite::Result<CachedStatement<'_>> {
        self.0.prepare_cached(sql)
    }
}

impl std::ops::Deref for Connection {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &rusqlite::Connection {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AuthError {
    #[error("not found")]
    NotFound,
    #[error("revision conflict")]
    Conflict,
    #[error("not authorized")]
    Denied,
    #[error("required authority unavailable")]
    RequiredMissing,
    #[error("displayed consent changed")]
    ConsentChanged,
    #[error("provisional client budget exhausted")]
    ResourceLimit,
    #[error("authorization session retired")]
    Retired,
    #[error("invalid authored input: {0}")]
    Invalid(String),
    #[error("authorization storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("authorization encoding failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("authorization protocol failure: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("authorization writer unavailable")]
    Unavailable,
    #[error("authorization broker effect failed: {0}")]
    Broker(String),
}

#[derive(Clone, Debug)]
pub(crate) struct AuthSettings {
    pub(crate) avatar_external_fetch_enabled: bool,
    /// Shortest accepted local password, in bytes.
    pub(crate) password_min_length: u16,
    pub(crate) administration: crate::config::AdministrationConfig,
    pub(crate) instance_id: String,
    pub(crate) account: String,
    pub(crate) oauth_origin: String,
    /// Origin that serves the Console, for links administrators open.
    pub(crate) console_origin: String,
    pub(crate) upstream_tls_roots_pem: Vec<String>,
    pub(crate) authority_lifetime_seconds: u32,
    pub(crate) clock_skew_seconds: u32,
    pub(crate) oauth_admission_lifetime_seconds: u32,
    pub(crate) login_lifetime_seconds: u32,
    pub(crate) enforcement_page_size: usize,
}

/// Auth's existing platform database owns all policy and effect transactions.
pub(crate) struct SqliteAuthorizationStore {
    /// Reached only through `writer`, which enforces the blocking-pool rule.
    connection: Mutex<rusqlite::Connection>,
    /// A read-only connection to the same WAL database, so reads neither wait
    /// for nor delay the writer. Without WAL, reads share the writer.
    reader: Option<Mutex<rusqlite::Connection>>,
    pub(super) signer: SigningKey,
    pub(super) custody_directory: PathBuf,
    current_key: Mutex<Option<(String, SigningKey)>>,
    /// Origins of every usable client's redirect URIs, answered from memory
    /// for CORS and rebuilt after any commit that changed a client row.
    client_origins: std::sync::RwLock<Arc<std::collections::BTreeSet<String>>>,
    /// Set by SQLite's update hook when a statement writes a client table.
    clients_changed: Arc<std::sync::atomic::AtomicBool>,
    /// Set by SQLite's update hook when a statement queues enforcement or effect work.
    work_queued: Arc<std::sync::atomic::AtomicBool>,
    /// Bumped after each commit that queued work, waking the effect workers.
    work_signal: tokio::sync::watch::Sender<u64>,
    pub(super) avatar_signer: std::sync::OnceLock<Arc<super::avatars::AvatarSigner>>,
    pub(crate) settings: AuthSettings,
}

impl std::fmt::Debug for SqliteAuthorizationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteAuthorizationStore")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// Aggregate mutation identity; replay returns the committed result, not effects.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Mutation {
    pub(crate) actor: String,
    pub(crate) request_id: String,
    pub(crate) request_digest: String,
    /// The caller's authorization session when an authenticated RPC made this
    /// mutation. A revocation covering it holds back only that session's own
    /// connection kicks briefly, so the caller still receives and verifies the
    /// reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
}

pub(super) fn json(value: &impl Serialize) -> Result<String, AuthError> {
    Ok(canonicalize_json(&serde_json::to_value(value)?)?)
}

pub(super) fn digest(value: &impl Serialize) -> Result<String, AuthError> {
    Ok(text_digest(&json(value)?))
}

/// Content address of already canonical JSON text.
pub(super) fn text_digest(canonical: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()))
}

pub(super) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, AuthError> {
    Ok(serde_json::from_str(value)?)
}

/// The wire timestamp for a stored Unix-seconds instant.
pub(super) fn timestamp(seconds: i64) -> Result<trellis_rs::generated::Timestamp, AuthError> {
    trellis_rs::generated::Timestamp::from_unix_seconds(seconds).map_err(|_| AuthError::Unavailable)
}

impl From<AuthError> for crate::supervisor::RuntimeError {
    fn from(error: AuthError) -> Self {
        Self::Platform(error.to_string())
    }
}

/// Run synchronous Auth storage work on Tokio's blocking pool.
///
/// Every store call takes the connection mutex and can wait behind password
/// hashing or IDL compilation, so async code reaches the store only through here.
/// Polled outside a Tokio runtime there is no async worker to protect, and the
/// work runs inline.
pub(crate) async fn blocking<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, E>
where
    T: Send + 'static,
    E: From<AuthError> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return work();
    }
    crate::blocking::spawn(work).await.unwrap_or_else(|error| {
        tracing::error!(%error, "Auth storage task failed");
        Err(AuthError::Unavailable.into())
    })
}

/// A locked read connection.
pub(crate) struct Reader<'a>(std::sync::MutexGuard<'a, rusqlite::Connection>);

impl std::ops::Deref for Reader<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        Connection::new(&self.0)
    }
}

/// Open a read-only companion connection when the database runs in WAL mode;
/// in any other journal mode a reader would block the writer's commits.
fn open_reader(
    connection: &rusqlite::Connection,
) -> Result<Option<rusqlite::Connection>, AuthError> {
    let Some(path) = connection.path().filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    let mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Ok(None);
    }
    let reader = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    reader.busy_timeout(std::time::Duration::from_secs(5))?;
    reader.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    Ok(Some(reader))
}

/// Origins of active registered clients' and validated CIMD clients' redirect URIs.
fn client_origins_in(
    connection: &Connection,
) -> Result<std::collections::BTreeSet<String>, AuthError> {
    let mut statement = connection.prepare(
        "SELECT j.value FROM auth_oauth_clients c,\
        json_each(c.redirect_uris_json) j \
        WHERE c.state='active' \
        UNION ALL SELECT j.value \
        FROM auth_cimd_clients c,json_each(c.metadata_json,'$.redirect_uris') \
        j \
        WHERE NOT EXISTS(SELECT 1 \
        FROM auth_oauth_clients r \
        WHERE r.client_id=c.client_id)",
    )?;
    let uris = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut origins = std::collections::BTreeSet::new();
    for uri in uris {
        if let Ok(uri) = url::Url::parse(&uri?) {
            origins.insert(uri.origin().ascii_serialization());
        }
    }
    Ok(origins)
}

/// The active issuer key that `signer` signs for.
pub(super) fn issuer_key(signer: &SigningKey) -> AuthorityIssuerKey {
    let key = signer.verifying_key();
    AuthorityIssuerKey {
        key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes())),
        public_key: URL_SAFE_NO_PAD.encode(key.as_bytes()),
        state: AuthorityIssuerState::Active,
    }
}

pub(super) fn id() -> String {
    ulid::Ulid::new().to_string()
}

pub(crate) fn read_custody_key(
    path: &std::path::Path,
) -> Result<[u8; 32], crate::supervisor::RuntimeError> {
    use crate::supervisor::RuntimeError;
    use std::io::Read as _;
    let invalid =
        |reason: &str| RuntimeError::Platform(format!("custody key '{}' {reason}", path.display()));
    let unreadable = |error: std::io::Error| invalid(&format!("cannot be read: {error}"));
    // Open without following a link and check the opened file, so a swap
    // between inspection and read cannot substitute another file.
    let metadata = std::fs::symlink_metadata(path).map_err(unreadable)?;
    if !metadata.is_file() {
        return Err(invalid("must be a regular file"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(unreadable)?;
    let opened = file.metadata().map_err(unreadable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid has no arguments or borrowed memory.
        let owner = unsafe { libc::geteuid() };
        if metadata.dev() != opened.dev()
            || metadata.ino() != opened.ino()
            || opened.mode() & 0o077 != 0
            || opened.uid() != owner
            || opened.nlink() != 1
        {
            return Err(invalid(
                "must be a single-link file owned by this user without group or world access",
            ));
        }
    }
    if !opened.is_file() || opened.len() > 128 {
        return Err(invalid("must be a regular file"));
    }
    let mut encoded = String::new();
    file.take(128)
        .read_to_string(&mut encoded)
        .map_err(unreadable)?;
    let encoded = encoded.trim();
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("is not base64url"))?;
    if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(invalid("is not canonical base64url"));
    }
    bytes
        .try_into()
        .map_err(|_| invalid("must contain exactly 32 bytes"))
}

impl SqliteAuthorizationStore {
    pub(crate) fn jetstream_identity(&self) -> Result<(String, bool), AuthError> {
        self.transaction(|connection| {
            Ok(connection.query_row(
                "SELECT jetstream_id,jetstream_bound FROM trellis_platform_store_marker WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
    }

    pub(crate) fn mark_jetstream_bound(&self, identity: &str) -> Result<(), AuthError> {
        self.transaction(|connection| {
            if connection.execute(
                "UPDATE trellis_platform_store_marker \
                SET jetstream_bound=1 \
                WHERE id=1 AND jetstream_id=?1",
                [identity],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            Ok(())
        })
    }

    /// Open the normally migrated platform database using provisioned custody.
    /// Startup never creates signing or OAuth-storage keys.
    pub(crate) fn open(
        store: &crate::storage::SqliteStore,
        config: &crate::RuntimeConfig,
        account: &str,
    ) -> Result<Self, crate::supervisor::RuntimeError> {
        Self::from_configured_connection(store.open()?, config, account)
    }

    pub(crate) fn open_read_only(
        store: &crate::storage::SqliteStore,
        config: &crate::RuntimeConfig,
        account: &str,
    ) -> Result<Self, crate::supervisor::RuntimeError> {
        Self::from_configured_connection(store.open_read_only()?, config, account)
    }

    fn from_configured_connection(
        connection: rusqlite::Connection,
        config: &crate::RuntimeConfig,
        account: &str,
    ) -> Result<Self, crate::supervisor::RuntimeError> {
        use crate::supervisor::RuntimeError;
        let authorization = config
            .resolve_authorization()
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let oauth = config
            .resolve_oauth_listener()
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let instance_id = config
            .auth
            .as_ref()
            .and_then(|auth| auth.instance_id.clone())
            .ok_or_else(|| RuntimeError::Platform("auth.instance_id is required".into()))?;
        let signer =
            SigningKey::from_bytes(&read_custody_key(&authorization.issuer_signing_seed_file)?);
        let local_identity = config
            .auth
            .as_ref()
            .and_then(|auth| auth.local_identity.as_ref());
        let settings = AuthSettings {
            avatar_external_fetch_enabled: config
                .auth
                .as_ref()
                .and_then(|auth| auth.avatar.as_ref())
                .is_none_or(|avatar| avatar.external_fetch_enabled),
            password_min_length: local_identity
                .and_then(|local| local.password_min_length)
                .unwrap_or(12),
            administration: config
                .auth
                .as_ref()
                .map(|auth| auth.administration)
                .unwrap_or_default(),
            instance_id,
            account: account.into(),
            oauth_origin: oauth.origin.clone(),
            console_origin: config.public_origin(),
            upstream_tls_roots_pem: Vec::new(),
            authority_lifetime_seconds: u32::try_from(authorization.context_lifetime_seconds)
                .map_err(|_| {
                    RuntimeError::Platform("authorization lifetime exceeds supported range".into())
                })?,
            clock_skew_seconds: u32::try_from(authorization.allowed_clock_skew_seconds).map_err(
                |_| {
                    RuntimeError::Platform(
                        "authorization clock skew exceeds supported range".into(),
                    )
                },
            )?,
            oauth_admission_lifetime_seconds: authorization
                .oauth_admission_lifetime_seconds
                .try_into()
                .map_err(|_| {
                    RuntimeError::Platform(
                        "OAuth admission lifetime exceeds supported range".into(),
                    )
                })?,
            login_lifetime_seconds: 86_400,
            enforcement_page_size: 128,
        };
        // Successor custody follows the writable, persistent Auth database, not
        // the provisioned seed that may live on a read-only configuration mount.
        let custody_directory = connection
            .path()
            .map(PathBuf::from)
            .ok_or_else(|| RuntimeError::Platform("Auth database path is unavailable".into()))?
            .with_extension("issuer-keys");
        let store = Self::from_connection(connection, signer, settings, custody_directory)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let initialized: bool = store
            .writer()
            .map_err(|_| RuntimeError::Platform("authorization writer unavailable".into()))?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_authorization_issuers)",
                [],
                |row| row.get(0),
            )
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        if initialized {
            store.issuer().map_err(|error| {
                RuntimeError::Platform(format!(
                    "cannot load committed authorization issuer custody from '{}': {error}; \
                    restore the matching signing-key backup",
                    store.custody_directory.display()
                ))
            })?;
        }
        Ok(store)
    }

    /// Takes an ordinarily initialized platform connection. Initialization and
    /// schema checks remain owned by the normal storage migration runner.
    pub(crate) fn from_connection(
        connection: rusqlite::Connection,
        signer: SigningKey,
        settings: AuthSettings,
        custody_directory: PathBuf,
    ) -> Result<Self, AuthError> {
        let origin = url::Url::parse(&settings.oauth_origin)
            .map_err(|_| AuthError::Invalid("invalid OAuth origin".into()))?;
        let loopback = matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if origin.origin().ascii_serialization() != settings.oauth_origin
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
            || (origin.scheme() != "https" && !(origin.scheme() == "http" && loopback))
        {
            return Err(AuthError::Invalid("unsafe OAuth origin".into()));
        }
        if settings.authority_lifetime_seconds == 0
            || settings.login_lifetime_seconds == 0
            || !(1..=256).contains(&settings.enforcement_page_size)
            || settings.instance_id.is_empty()
            || settings.account.is_empty()
        {
            return Err(AuthError::Invalid(
                "invalid authority limits or scope".into(),
            ));
        }
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
        let clients_changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let work_queued = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (changed, queued) = (Arc::clone(&clients_changed), Arc::clone(&work_queued));
        connection.update_hook(Some(
            move |action: rusqlite::hooks::Action, _: &str, table: &str, _: i64| match table {
                "auth_oauth_clients" | "auth_cimd_clients" => {
                    changed.store(true, std::sync::atomic::Ordering::Release);
                }
                "auth_enforcement_work" | "auth_post_commit_actions"
                    if action == rusqlite::hooks::Action::SQLITE_INSERT =>
                {
                    queued.store(true, std::sync::atomic::Ordering::Release);
                }
                _ => {}
            },
        ))?;
        let client_origins = client_origins_in(Connection::new(&connection))?;
        let reader = open_reader(&connection)?.map(Mutex::new);
        Ok(Self {
            connection: Mutex::new(connection),
            reader,
            signer,
            custody_directory,
            current_key: Mutex::new(None),
            client_origins: std::sync::RwLock::new(Arc::new(client_origins)),
            clients_changed,
            work_queued,
            work_signal: tokio::sync::watch::Sender::new(0),
            avatar_signer: std::sync::OnceLock::new(),
            settings,
        })
    }

    fn initial_issuer(&self) -> AuthorityIssuerKey {
        issuer_key(&self.signer)
    }

    pub(crate) fn issuer(&self) -> Result<AuthorityIssuerKey, AuthError> {
        let connection = self.writer()?;
        self.current_issuer(Connection::new(&connection))
    }

    pub(super) fn current_issuer(
        &self,
        connection: &Connection,
    ) -> Result<AuthorityIssuerKey, AuthError> {
        Ok(issuer_key(&self.current_signer(connection)?))
    }

    /// Capture the configured native trust pin and its persisted rotation position.
    pub(crate) fn runtime_authorization_trust(
        &self,
    ) -> Result<trellis_rs::client::RuntimeAuthorizationTrust, AuthError> {
        let connection = self.writer()?;
        let connection = Connection::new(&connection);
        let issuer = self.current_issuer(connection)?;
        let (current, rotation_sequence): (bool, u64) = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_authorization_issuers \
            WHERE key_id=?1 AND public_key=?2 \
            AND is_current=1 AND state='active' \
            AND revoked_at IS NULL), coalesce((SELECT max(sequence) \
            FROM auth_issuer_rotations \
            WHERE trellis_instance_id=?3 \
            AND audience_nats_account=?4),0)",
            params![
                issuer.key_id,
                issuer.public_key,
                self.settings.instance_id,
                self.settings.account
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !current {
            return Err(AuthError::Denied);
        }
        Ok(trellis_rs::client::RuntimeAuthorizationTrust {
            instance_id: self.settings.instance_id.clone(),
            account: self.settings.account.clone(),
            issuer,
            rotation_sequence,
            root_certificates: Vec::new(),
        })
    }

    pub(super) fn transaction<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        let mut connection = self.writer()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = operation(Connection::new(&transaction))?;
        transaction.commit()?;
        self.after_commit(&connection);
        Ok(result)
    }

    /// `transaction` for async callers, run on the blocking pool.
    pub(crate) async fn transaction_blocking<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&Connection) -> Result<T, AuthError> + Send + 'static,
    ) -> Result<T, AuthError> {
        let store = Arc::clone(self);
        blocking(move || store.transaction(operation)).await
    }

    /// Refresh state derived from committed rows. Runs while the committing
    /// caller still holds the connection, so no other write can interleave.
    pub(super) fn after_commit(&self, connection: &rusqlite::Connection) {
        use std::sync::atomic::Ordering;
        if self.work_queued.swap(false, Ordering::AcqRel) {
            self.work_signal
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        if !self.clients_changed.swap(false, Ordering::AcqRel) {
            return;
        }
        match client_origins_in(Connection::new(connection)) {
            Ok(origins) => match self.client_origins.write() {
                Ok(mut current) => *current = Arc::new(origins),
                Err(_) => self.clients_changed.store(true, Ordering::Release),
            },
            Err(error) => {
                tracing::error!(%error, "cannot refresh OAuth client origins");
                self.clients_changed.store(true, Ordering::Release);
            }
        }
    }

    /// Changes whenever a commit queues enforcement or effect work.
    pub(crate) fn work_signal(&self) -> tokio::sync::watch::Receiver<u64> {
        self.work_signal.subscribe()
    }

    /// Whether `origin` serves a redirect URI of a usable client.
    pub(crate) fn allows_client_origin(&self, origin: &str) -> bool {
        self.client_origins
            .read()
            .is_ok_and(|origins| origins.contains(origin))
    }

    /// A consistent snapshot for work that never writes. It takes no write
    /// lock, so it does not wait for or delay writers.
    pub(super) fn read<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        let mut connection = self.reader()?;
        let transaction = connection
            .0
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        operation(Connection::new(&transaction))
    }

    /// The writer connection. Every write and every transaction locks it here.
    pub(super) fn writer(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>, AuthError> {
        crate::blocking::assert_off_async_workers();
        self.connection.lock().map_err(|_| AuthError::Unavailable)
    }

    /// The connection for a single read-only statement outside a transaction.
    pub(super) fn reader(&self) -> Result<Reader<'_>, AuthError> {
        crate::blocking::assert_off_async_workers();
        self.reader
            .as_ref()
            .unwrap_or(&self.connection)
            .lock()
            .map(Reader)
            .map_err(|_| AuthError::Unavailable)
    }

    pub(super) fn current_signer(&self, connection: &Connection) -> Result<SigningKey, AuthError> {
        let current: Option<String> = connection
            .query_row(
                "SELECT key_id FROM auth_authorization_issuers \
                WHERE is_current=1 AND state='active' \
                AND revoked_at IS NULL",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let current = current.ok_or(AuthError::Denied)?;
        if current == self.initial_issuer().key_id {
            return Ok(self.signer.clone());
        }
        let mut cached = self
            .current_key
            .lock()
            .map_err(|_| AuthError::Unavailable)?;
        if let Some((key_id, key)) = cached.as_ref() {
            if key_id == &current {
                return Ok(key.clone());
            }
        }
        // Persisted IDs are content addresses, never arbitrary filesystem paths.
        let bytes = URL_SAFE_NO_PAD
            .decode(&current)
            .map_err(|_| AuthError::Denied)?;
        if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != current {
            return Err(AuthError::Denied);
        }
        let metadata = std::fs::symlink_metadata(&self.custody_directory)
            .map_err(|_| AuthError::Unavailable)?;
        if !metadata.is_dir() {
            return Err(AuthError::Denied);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(AuthError::Denied);
            }
        }
        let key = SigningKey::from_bytes(
            &read_custody_key(&self.custody_directory.join(format!("{current}.key")))
                .map_err(|error| {
                    tracing::error!(%error, key_id = %current, "cannot load committed issuer signing custody");
                    AuthError::Unavailable
                })?,
        );
        if URL_SAFE_NO_PAD.encode(Sha256::digest(key.verifying_key().as_bytes())) != current {
            return Err(AuthError::Denied);
        }
        *cached = Some((current, key.clone()));
        Ok(key)
    }

    pub(super) fn persist_successor(&self, key_id: &str, seed: &[u8; 32]) -> Result<(), AuthError> {
        use std::{fs, io::Write};
        let write = || -> std::io::Result<()> {
            let mut directory = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory.mode(0o700);
            }
            match directory.create(&self.custody_directory) {
                Ok(()) => {
                    #[cfg(unix)]
                    fs::File::open(
                        self.custody_directory
                            .parent()
                            .ok_or_else(|| std::io::Error::other("missing custody parent"))?,
                    )?
                    .sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = fs::symlink_metadata(&self.custody_directory)?;
            if !metadata.is_dir() {
                return Err(std::io::Error::other("unsafe issuer custody directory"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(std::io::Error::other(
                        "issuer custody directory is not private",
                    ));
                }
            }
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(self.custody_directory.join(format!("{key_id}.key")))?;
            writeln!(file, "{}", URL_SAFE_NO_PAD.encode(seed))?;
            file.sync_all()?;
            #[cfg(unix)]
            fs::File::open(&self.custody_directory)?.sync_all()?;
            Ok(())
        };
        write().map_err(|error| {
            tracing::error!(%error, key_id, "cannot persist successor issuer signing custody");
            AuthError::Unavailable
        })
    }

    pub(super) fn mutate<T: Serialize + DeserializeOwned>(
        &self,
        identity: &Mutation,
        purpose: &str,
        input: &impl Serialize,
        now: i64,
        operation: impl FnOnce(&Connection) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        self.transaction(|connection| {
            self.mutate_in_transaction(connection, identity, purpose, input, now, operation)
        })
    }

    pub(super) fn mutate_in_transaction<T: Serialize + DeserializeOwned>(
        &self,
        connection: &Connection,
        identity: &Mutation,
        purpose: &str,
        input: &impl Serialize,
        now: i64,
        operation: impl FnOnce(&Connection) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        if identity.actor.is_empty()
            || ulid::Ulid::from_string(&identity.request_id).is_err()
            || identity.request_digest != digest(input)?
        {
            return Err(AuthError::Invalid("invalid mutation identity".into()));
        }
        let previous: Option<(String, String)> = connection
            .query_row(
                "SELECT request_digest,result_json \
                FROM auth_idempotency_results \
                WHERE purpose=?1 AND signer_id=?2 \
                AND request_id=?3",
                params![purpose, identity.actor, identity.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous_digest, result)) = previous {
            if previous_digest != identity.request_digest {
                return Err(AuthError::Conflict);
            }
            return decode(&result);
        }
        let result = operation(connection)?;
        connection.execute(
            "INSERT INTO auth_idempotency_results(scope_key,purpose,signer_id,\
            request_id,request_digest,result_json,created_at,expires_at) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                digest(&(purpose, &identity.actor, &identity.request_id))?,
                purpose,
                identity.actor,
                identity.request_id,
                identity.request_digest,
                json(&result)?,
                now,
                now.saturating_add(2_592_000)
            ],
        )?;
        connection.execute(
            "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,\
            target_id,data_json,created_at) \
            VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                id(),
                identity.actor,
                purpose,
                identity.request_id,
                json(input)?,
                now
            ],
        )?;
        Ok(result)
    }

    /// Trusted startup initializes only a new key. An existing pin cannot be
    /// replaced by configuring an unrelated private key on the next restart.
    pub(crate) fn initialize_issuer(&self, now: i64) -> Result<(), AuthError> {
        self.transaction(|connection| {
            let current: Option<String> = connection
                .query_row(
                    "SELECT key_id FROM auth_authorization_issuers WHERE is_current=1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let issuer = self.initial_issuer();
            match current {
                Some(_) => self.current_signer(connection).map(|_| ()),
                None => {
                    let count: i64 = connection.query_row(
                        "SELECT count(*) FROM auth_authorization_issuers",
                        [],
                        |row| row.get(0),
                    )?;
                    if count != 0 {
                        return Err(AuthError::Conflict);
                    }
                    connection.execute(
                        "INSERT INTO auth_authorization_issuers(key_id,public_key,is_current,\
                        state,activated_at,maximum_acceptance_deadline,created_at) \
                        VALUES(?1,?2,1,'active',?3,?3,?3)",
                        params![issuer.key_id, issuer.public_key, now],
                    )?;
                    Ok(())
                }
            }
        })
    }

    pub(super) fn policy_changed(
        connection: &Connection,
        scope: &EnforcementScope,
        now: i64,
    ) -> Result<String, AuthError> {
        enqueue_scope(connection, scope, now)
    }
}
