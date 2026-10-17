use std::{path::PathBuf, sync::Mutex};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    canonicalize_json, AuthorityIssuerKey, AuthorityIssuerState, ProtocolError,
};

use super::revocation::{enqueue_scope, EnforcementScope};

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
    pub(crate) instance_id: String,
    pub(crate) account: String,
    pub(crate) oauth_origin: String,
    pub(crate) upstream_tls_roots_pem: Vec<String>,
    pub(crate) authority_lifetime_seconds: u32,
    pub(crate) clock_skew_seconds: u32,
    pub(crate) login_lifetime_seconds: u32,
    pub(crate) enforcement_page_size: usize,
}

/// Auth's existing platform database owns all policy and effect transactions.
pub(crate) struct SqliteAuthorizationStore {
    pub(super) connection: Mutex<Connection>,
    pub(super) signer: SigningKey,
    pub(super) custody_directory: PathBuf,
    current_key: Mutex<Option<(String, SigningKey)>>,
    pub(super) settings: AuthSettings,
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
}

pub(super) fn json(value: &impl Serialize) -> Result<String, AuthError> {
    Ok(canonicalize_json(&serde_json::to_value(value)?)?)
}

pub(super) fn digest(value: &impl Serialize) -> Result<String, AuthError> {
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(json(value)?.as_bytes())))
}

pub(super) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, AuthError> {
    Ok(serde_json::from_str(value)?)
}

pub(super) fn id() -> String {
    ulid::Ulid::new().to_string()
}

pub(crate) fn read_custody_key(
    path: &std::path::Path,
) -> Result<[u8; 32], crate::supervisor::RuntimeError> {
    use crate::supervisor::RuntimeError;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        RuntimeError::Platform(format!(
            "cannot inspect custody key '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(RuntimeError::Platform(format!(
            "custody key '{}' must be a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RuntimeError::Platform(format!(
                "custody key '{}' must not be group/world accessible",
                path.display()
            )));
        }
    }
    let encoded = std::fs::read_to_string(path).map_err(|error| {
        RuntimeError::Platform(format!(
            "cannot read custody key '{}': {error}",
            path.display()
        ))
    })?;
    let encoded = encoded.trim();
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| RuntimeError::Platform(format!("invalid custody key '{}'", path.display())))?;
    if URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(RuntimeError::Platform(format!(
            "noncanonical custody key '{}'",
            path.display()
        )));
    }
    bytes.try_into().map_err(|_| {
        RuntimeError::Platform(format!(
            "custody key '{}' must contain exactly 32 bytes",
            path.display()
        ))
    })
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
            if connection.execute("UPDATE trellis_platform_store_marker SET jetstream_bound=1 WHERE id=1 AND jetstream_id=?1",[identity])?!=1 {return Err(AuthError::Conflict);}
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
        connection: Connection,
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
        let settings = AuthSettings {
            instance_id,
            account: account.into(),
            oauth_origin: oauth.origin.clone(),
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
            .connection
            .lock()
            .map_err(|_| RuntimeError::Platform("authorization writer unavailable".into()))?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_authorization_issuers)",
                [],
                |row| row.get(0),
            )
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        if initialized {
            store
                .issuer()
                .map_err(|error| RuntimeError::Platform(format!("cannot load committed authorization issuer custody from '{}': {error}; restore the matching signing-key backup", store.custody_directory.display())))?;
        }
        Ok(store)
    }

    /// Takes an ordinarily initialized platform connection. Initialization and
    /// schema checks remain owned by the normal storage migration runner.
    pub(crate) fn from_connection(
        connection: Connection,
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
        Ok(Self {
            connection: Mutex::new(connection),
            signer,
            custody_directory,
            current_key: Mutex::new(None),
            settings,
        })
    }

    fn initial_issuer(&self) -> AuthorityIssuerKey {
        let key = self.signer.verifying_key();
        AuthorityIssuerKey {
            key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes())),
            public_key: URL_SAFE_NO_PAD.encode(key.as_bytes()),
            state: AuthorityIssuerState::Active,
        }
    }

    pub(crate) fn issuer(&self) -> Result<AuthorityIssuerKey, AuthError> {
        let connection = self.connection.lock().map_err(|_| AuthError::Unavailable)?;
        self.current_issuer(&connection)
    }

    pub(super) fn current_issuer(
        &self,
        connection: &Connection,
    ) -> Result<AuthorityIssuerKey, AuthError> {
        let signer = self.current_signer(connection)?;
        let key = signer.verifying_key();
        Ok(AuthorityIssuerKey {
            key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes())),
            public_key: URL_SAFE_NO_PAD.encode(key.as_bytes()),
            state: AuthorityIssuerState::Active,
        })
    }

    /// Capture the configured native trust pin and its persisted rotation position.
    pub(crate) fn runtime_authorization_trust(
        &self,
    ) -> Result<trellis_rs::client::RuntimeAuthorizationTrust, AuthError> {
        let connection = self.connection.lock().map_err(|_| AuthError::Unavailable)?;
        let issuer = self.current_issuer(&connection)?;
        let (current,rotation_sequence):(bool,u64)=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_authorization_issuers WHERE key_id=?1 AND public_key=?2 AND is_current=1 AND state='active' AND revoked_at IS NULL), coalesce((SELECT max(sequence) FROM auth_issuer_rotations WHERE trellis_instance_id=?3 AND audience_nats_account=?4),0)",params![issuer.key_id,issuer.public_key,self.settings.instance_id,self.settings.account],|row|Ok((row.get(0)?,row.get(1)?)))?;
        if !current {
            return Err(AuthError::Denied);
        }
        Ok(trellis_rs::client::RuntimeAuthorizationTrust {
            instance_id: self.settings.instance_id.clone(),
            account: self.settings.account.clone(),
            issuer,
            rotation_sequence,
        })
    }

    pub(super) fn transaction<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        let mut connection = self.connection.lock().map_err(|_| AuthError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(super) fn current_signer(&self, connection: &Connection) -> Result<SigningKey, AuthError> {
        let current: Option<String> = connection.query_row("SELECT key_id FROM auth_authorization_issuers WHERE is_current=1 AND state='active' AND revoked_at IS NULL", [], |row| row.get(0)).optional()?;
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
        let previous: Option<(String, String)> = connection.query_row(
                "SELECT request_digest,result_json FROM auth_idempotency_results WHERE purpose=?1 AND signer_id=?2 AND request_id=?3",
                params![purpose, identity.actor, identity.request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        if let Some((previous_digest, result)) = previous {
            if previous_digest != identity.request_digest {
                return Err(AuthError::Conflict);
            }
            return decode(&result);
        }
        let result = operation(connection)?;
        connection.execute("INSERT INTO auth_idempotency_results(scope_key,purpose,signer_id,request_id,request_digest,result_json,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![digest(&(purpose, &identity.actor, &identity.request_id))?,purpose,identity.actor,identity.request_id,identity.request_digest,json(&result)?,now,now.saturating_add(2_592_000)])?;
        connection.execute("INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,target_id,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id(),identity.actor,purpose,identity.request_id,json(input)?,now])?;
        Ok(result)
    }

    /// Trusted startup initializes only a new key. An existing pin cannot be
    /// replaced by configuring an unrelated private key on the next restart.
    pub(crate) fn initialize_issuer(&self, now: i64) -> Result<(), AuthError> {
        self.transaction(|connection| {
            let current: Option<String> = connection.query_row("SELECT key_id FROM auth_authorization_issuers WHERE is_current=1", [], |row| row.get(0)).optional()?;
            let issuer = self.initial_issuer();
            match current {
                Some(_) => self.current_signer(connection).map(|_| ()),
                None => {
                    let count: i64 = connection.query_row("SELECT count(*) FROM auth_authorization_issuers", [], |row| row.get(0))?;
                    if count != 0 { return Err(AuthError::Conflict); }
                    connection.execute("INSERT INTO auth_authorization_issuers(key_id,public_key,is_current,state,activated_at,maximum_acceptance_deadline,created_at) VALUES(?1,?2,1,'active',?3,?3,?3)", params![issuer.key_id,issuer.public_key,now])?;
                    Ok(())
                }
            }
        })
    }

    pub(super) fn require_privilege(
        connection: &Connection,
        actor: &str,
        privilege: &str,
    ) -> Result<(), AuthError> {
        let permitted: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_platform_privileges p JOIN auth_principals u USING(principal_id) WHERE p.principal_id=?1 AND p.privilege=?2 AND u.state='active')", params![actor,privilege], |row| row.get(0))?;
        if permitted {
            Ok(())
        } else {
            Err(AuthError::Denied)
        }
    }

    pub(super) fn policy_changed(
        connection: &Connection,
        scope: &EnforcementScope,
        now: i64,
    ) -> Result<String, AuthError> {
        enqueue_scope(connection, scope, now)
    }
}
