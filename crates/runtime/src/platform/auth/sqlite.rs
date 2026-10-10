use std::sync::Mutex;

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
    pub(crate) authority_lifetime_seconds: u32,
    pub(crate) clock_skew_seconds: u32,
    pub(crate) enforcement_page_size: usize,
}

/// Auth's existing platform database owns all policy and effect transactions.
pub(crate) struct SqliteAuthorizationStore {
    pub(super) connection: Mutex<Connection>,
    pub(super) signer: SigningKey,
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

impl SqliteAuthorizationStore {
    /// Takes an ordinarily initialized platform connection. Initialization and
    /// schema checks remain owned by the normal storage migration runner.
    pub(crate) fn from_connection(
        connection: Connection,
        signer: SigningKey,
        settings: AuthSettings,
    ) -> Result<Self, AuthError> {
        if settings.authority_lifetime_seconds == 0
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
            settings,
        })
    }

    pub(crate) fn issuer(&self) -> AuthorityIssuerKey {
        let key = self.signer.verifying_key();
        AuthorityIssuerKey {
            key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes())),
            public_key: URL_SAFE_NO_PAD.encode(key.as_bytes()),
            state: AuthorityIssuerState::Active,
        }
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

    pub(super) fn current_signer(&self, connection: &Connection) -> Result<(), AuthError> {
        let current: Option<String> = connection.query_row("SELECT key_id FROM auth_authorization_issuers WHERE is_current=1 AND state='active' AND revoked_at IS NULL", [], |row| row.get(0)).optional()?;
        if current.as_deref() != Some(&self.issuer().key_id) {
            return Err(AuthError::Denied);
        }
        Ok(())
    }

    pub(super) fn mutate<T: Serialize + DeserializeOwned>(
        &self,
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
        self.transaction(|connection| {
            let previous: Option<(String, String)> = connection.query_row(
                "SELECT request_digest,result_json FROM auth_idempotency_results WHERE purpose=?1 AND signer_id=?2 AND request_id=?3",
                params![purpose, identity.actor, identity.request_id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            if let Some((previous_digest, result)) = previous {
                if previous_digest != identity.request_digest { return Err(AuthError::Conflict); }
                return decode(&result);
            }
            let result = operation(connection)?;
            connection.execute("INSERT INTO auth_idempotency_results(scope_key,purpose,signer_id,request_id,request_digest,result_json,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![digest(&(purpose, &identity.actor, &identity.request_id))?,purpose,identity.actor,identity.request_id,identity.request_digest,json(&result)?,now,now.saturating_add(2_592_000)])?;
            connection.execute("INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,target_id,data_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id(),identity.actor,purpose,identity.request_id,json(input)?,now])?;
            Ok(result)
        })
    }

    /// Trusted startup initializes only a new key. An existing pin cannot be
    /// replaced by configuring an unrelated private key on the next restart.
    pub(crate) fn initialize_issuer(&self, now: i64) -> Result<(), AuthError> {
        self.transaction(|connection| {
            let current: Option<String> = connection.query_row("SELECT key_id FROM auth_authorization_issuers WHERE is_current=1", [], |row| row.get(0)).optional()?;
            let issuer = self.issuer();
            match current {
                Some(key) if key == issuer.key_id => self.current_signer(connection),
                Some(_) => Err(AuthError::Conflict),
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
