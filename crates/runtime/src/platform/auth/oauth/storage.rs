//! Durable OAuth engine records share the authoritative Auth database. Secrets
//! are AEAD-sealed with an installation-owned key, and indexed only by hash.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use oauth_as::authorization::{AuthorizationCodeRecord, AuthorizationCodeState};
use oauth_as::client::{Client, ClientAuth, ClientId};
use oauth_as::device::{normalize_user_code, DeviceGrant, DeviceGrantState};
use oauth_as::store::{RevocationWindow, Storage, StorageError, WriteOutcome};
use oauth_as::token::{IssuedToken, RefreshTokenRecord};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::revocation::enqueue_scope;
use super::super::{EnforcementScope, SqliteAuthorizationStore};

pub(crate) struct OAuthStorage {
    pub(crate) authority: Arc<SqliteAuthorizationStore>,
    sealing_key: LessSafeKey,
}

struct Record<'a, T> {
    kind: &'static str,
    key: &'a str,
    value: &'a T,
    client: Option<&'a str>,
    grant: Option<&'a str>,
    family: Option<&'a str>,
    user_code: Option<String>,
    established: SystemTime,
    expires: Option<SystemTime>,
}

#[derive(Serialize, Deserialize)]
struct Sealed {
    nonce: String,
    ciphertext: String,
}

fn hash(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

fn seconds(value: SystemTime) -> Result<i64, StorageError> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or_else(|| StorageError::new("OAuth record time outside supported range"))
}

fn nanos(value: SystemTime) -> Result<i64, StorageError> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .ok_or_else(|| StorageError::new("OAuth record time outside supported range"))
}

fn failure(error: impl std::fmt::Display) -> StorageError {
    StorageError::new(error.to_string())
}

impl OAuthStorage {
    /// Remove a bounded page of expired protocol secrets without deleting
    /// authority history or refresh-reuse evidence for a still-live family.
    pub(crate) fn cleanup_expired(&self, now: i64) -> Result<usize, StorageError> {
        self.transaction(|connection| {
            connection.execute(
                "DELETE FROM auth_oauth_artifacts WHERE rowid IN (SELECT a.rowid FROM auth_oauth_artifacts a WHERE a.expires_at<=?1 AND (a.kind!='refresh' OR NOT EXISTS(SELECT 1 FROM auth_oauth_families f WHERE f.family_lookup_hash=a.family_lookup_hash AND f.expires_at>?1)) ORDER BY a.expires_at,a.rowid LIMIT 256)",
                [now],
            ).map_err(failure)
        })
    }

    /// Open durable OAuth custody using provisioned installation material. Never
    /// generate or replace a key during startup: losing it loses stored secrets.
    pub(crate) fn open(
        authority: Arc<SqliteAuthorizationStore>,
        path: &std::path::Path,
    ) -> Result<Self, StorageError> {
        use std::io::Read as _;
        let metadata = std::fs::symlink_metadata(path).map_err(failure)?;
        if !metadata.is_file() {
            return Err(StorageError::new(
                "OAuth storage key must be a regular file",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(failure)?;
        let opened = file.metadata().map_err(failure)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            // SAFETY: geteuid has no arguments or borrowed memory.
            let owner = unsafe { libc::geteuid() };
            if metadata.dev() != opened.dev()
                || metadata.ino() != opened.ino()
                || opened.mode() & 0o777 != 0o600
                || opened.uid() != owner
                || opened.nlink() != 1
            {
                return Err(StorageError::new(
                    "OAuth storage key must be an owner-only 0600 file",
                ));
            }
        }
        if !opened.is_file() || opened.len() > 128 {
            return Err(StorageError::new(
                "OAuth storage key must be a regular file",
            ));
        }
        let mut encoded = String::new();
        file.take(128)
            .read_to_string(&mut encoded)
            .map_err(failure)?;
        let key: [u8; 32] = URL_SAFE_NO_PAD
            .decode(encoded.trim())
            .map_err(failure)?
            .try_into()
            .map_err(|_| StorageError::new("OAuth storage key must contain 32 bytes"))?;
        let storage = Self::new(authority, &key)?;
        // Authenticate real persisted ciphertext before accepting new secrets.
        // A replaced but well-formed key must not silently split installation
        // custody between two keys. No synthetic key-check record is needed.
        storage.transaction(|connection| {
            let artifact: Option<(String, String, Vec<u8>)> = connection.query_row(
                "SELECT kind,lookup_hash,sealed_record FROM auth_oauth_artifacts LIMIT 1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional().map_err(failure)?;
            if let Some((kind, lookup, bytes)) = artifact {
                storage.decode::<serde_json::Value>(&kind, &lookup, &bytes)?;
            } else {
                let provider: Option<(String, Vec<u8>)> = connection.query_row(
                    "SELECT provider_id,sealed_client_secret FROM auth_oidc_providers WHERE sealed_client_secret IS NOT NULL LIMIT 1",
                    [], |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional().map_err(failure)?;
                if let Some((provider, bytes)) = provider {
                    storage.decode::<String>("upstreamSecret", &hash(&provider), &bytes)?;
                }
            }
            Ok(())
        })?;
        Ok(storage)
    }

    pub(super) fn store_upstream(
        &self,
        state: &str,
        flow: &super::oidc::UpstreamFlow,
        client: &str,
    ) -> Result<(), StorageError> {
        self.transaction(|connection| {
            self.write(
                connection,
                Record {
                    kind: "upstream",
                    key: state,
                    value: flow,
                    client: Some(client),
                    grant: None,
                    family: None,
                    user_code: None,
                    established: SystemTime::now(),
                    expires: Some(
                        UNIX_EPOCH
                            + std::time::Duration::from_secs(
                                flow.expires_at.try_into().map_err(failure)?,
                            ),
                    ),
                },
            )
        })
    }

    pub(super) fn take_upstream(
        &self,
        state: &str,
        session: &str,
        now: i64,
    ) -> Result<super::oidc::UpstreamFlow, StorageError> {
        self.transaction(|connection| {
            let flow: super::oidc::UpstreamFlow = self
                .read(connection, "upstream", state)?
                .ok_or_else(|| StorageError::new("upstream authorization unavailable"))?;
            if flow.session_id != session || flow.expires_at <= now {
                return Err(StorageError::new("upstream authorization unavailable"));
            }
            connection
                .execute(
                    "DELETE FROM auth_oauth_artifacts WHERE kind='upstream' AND lookup_hash=?1",
                    [hash(state)],
                )
                .map_err(failure)?;
            Ok(flow)
        })
    }

    /// Bind a configured upstream secret to its exact provider identity. The
    /// administrative API seals before its normal registration transaction.
    pub(crate) fn seal_upstream_client_secret(
        &self,
        provider: &str,
        secret: &str,
    ) -> Result<Vec<u8>, StorageError> {
        self.encode("upstreamSecret", &hash(provider), &secret)
    }

    pub(super) fn upstream_client_secret(
        &self,
        provider: &str,
        sealed: Option<&[u8]>,
    ) -> Result<Option<openidconnect::ClientSecret>, StorageError> {
        sealed
            .map(|sealed| {
                self.decode::<String>("upstreamSecret", &hash(provider), sealed)
                    .map(openidconnect::ClientSecret::new)
            })
            .transpose()
    }

    pub(super) fn store_device_selection(
        &self,
        grant: &DeviceGrant,
        selection: &super::device::DeviceSelection,
    ) -> Result<(), StorageError> {
        self.transaction(|connection| {
            self.write(
                connection,
                Record {
                    kind: "deviceSelection",
                    key: &grant.device_code,
                    value: selection,
                    client: Some(grant.client_id.as_str()),
                    grant: None,
                    family: None,
                    user_code: None,
                    established: grant.created_at,
                    expires: Some(grant.expires_at),
                },
            )
        })
    }

    pub(super) fn device_selection(
        &self,
        code: &str,
    ) -> Result<super::device::DeviceSelection, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        self.read(&connection, "deviceSelection", code)?
            .ok_or_else(|| StorageError::new("device request unavailable"))
    }

    /// Persists validated public metadata without creating a registered client.
    /// Existing registration tombstones cannot be bypassed by CIMD retrieval.
    pub(super) fn install_client_document(
        &self,
        document: &super::ClientDocument,
    ) -> Result<(), StorageError> {
        let client = document.document.to_client();
        self.transaction(|connection| {
            let registered: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1)", [client.client_id.as_str()], |row|row.get(0)).map_err(failure)?;
            if registered {return Err(StorageError::new("client registration changed"));}
            self.write(connection, Record {kind:"client",key:client.client_id.as_str(),value:&client,client:Some(client.client_id.as_str()),grant:None,family:None,user_code:None,established:SystemTime::now(),expires:None})?;
            connection.execute("INSERT INTO auth_cimd_clients VALUES(?1,?2,?3,?4,?5) ON CONFLICT(client_id) DO UPDATE SET kind=excluded.kind,metadata_digest=excluded.metadata_digest,metadata_json=excluded.metadata_json,observed_at=excluded.observed_at",params![client.client_id.as_str(),document.kind,document.revision,serde_json::to_string(&document.metadata).map_err(failure)?,seconds(SystemTime::now())?]).map_err(failure)?;
            Ok(())
        })
    }

    pub(super) fn store_authorization(
        &self,
        id: &str,
        flow: &super::portal::PortalAuthorization,
        client: &str,
    ) -> Result<(), StorageError> {
        self.transaction(|connection| {
            self.write(
                connection,
                Record {
                    kind: "authorization",
                    key: id,
                    value: flow,
                    client: Some(client),
                    grant: None,
                    family: None,
                    user_code: None,
                    established: SystemTime::now(),
                    expires: Some(
                        UNIX_EPOCH
                            + std::time::Duration::from_secs(
                                flow.expires_at.try_into().map_err(failure)?,
                            ),
                    ),
                },
            )
        })
    }

    pub(super) fn take_authorization(
        &self,
        id: &str,
        session: &str,
        now: i64,
    ) -> Result<super::portal::PortalAuthorization, StorageError> {
        self.transaction(|connection| {
            let flow: super::portal::PortalAuthorization = self.read(connection, "authorization", id)?.ok_or_else(||StorageError::new("authorization unavailable"))?;
            if flow.session_id != session || flow.expires_at <= now {return Err(StorageError::new("authorization unavailable"));}
            connection.execute("DELETE FROM auth_oauth_artifacts WHERE kind='authorization' AND lookup_hash=?1", [hash(id)]).map_err(failure)?;
            Ok(flow)
        })
    }

    pub(super) fn authorization(
        &self,
        id: &str,
        session: &str,
        now: i64,
    ) -> Result<super::portal::PortalAuthorization, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        let flow: super::portal::PortalAuthorization = self
            .read(&connection, "authorization", id)?
            .ok_or_else(|| StorageError::new("authorization unavailable"))?;
        if flow.session_id != session || flow.expires_at <= now {
            return Err(StorageError::new("authorization unavailable"));
        }
        Ok(flow)
    }

    pub(crate) fn new(
        authority: Arc<SqliteAuthorizationStore>,
        key: &[u8; 32],
    ) -> Result<Self, StorageError> {
        Ok(Self {
            authority,
            sealing_key: LessSafeKey::new(
                UnboundKey::new(&AES_256_GCM, key)
                    .map_err(|_| StorageError::new("invalid OAuth sealing key"))?,
            ),
        })
    }

    fn transaction<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let mut connection = self.authority.connection.lock().map_err(failure)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(failure)?;
        let result = action(&transaction)?;
        transaction.commit().map_err(failure)?;
        Ok(result)
    }

    fn decode<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
        sealed: &[u8],
    ) -> Result<T, StorageError> {
        let sealed: Sealed = serde_json::from_slice(sealed).map_err(failure)?;
        let nonce: [u8; 12] = URL_SAFE_NO_PAD
            .decode(sealed.nonce)
            .map_err(failure)?
            .try_into()
            .map_err(|_| StorageError::new("invalid OAuth record nonce"))?;
        let mut ciphertext = URL_SAFE_NO_PAD.decode(sealed.ciphertext).map_err(failure)?;
        let associated = format!("{kind}:{key}");
        let plaintext = self
            .sealing_key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(associated.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| StorageError::new("OAuth record authentication failed"))?;
        serde_json::from_slice(plaintext).map_err(failure)
    }

    fn read<T: DeserializeOwned>(
        &self,
        connection: &Connection,
        kind: &str,
        key: &str,
    ) -> Result<Option<T>, StorageError> {
        let key = hash(key);
        let sealed: Option<Vec<u8>> = connection
            .query_row(
                "SELECT sealed_record FROM auth_oauth_artifacts WHERE kind=?1 AND lookup_hash=?2",
                params![kind, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(failure)?;
        sealed
            .map(|sealed| self.decode(kind, &key, &sealed))
            .transpose()
    }

    fn write<T: Serialize>(
        &self,
        connection: &Connection,
        record: Record<'_, T>,
    ) -> Result<(), StorageError> {
        let key = hash(record.key);
        let sealed = self.encode(record.kind, &key, record.value)?;
        let family = record.family.map(hash);
        if let (Some(family), Some(grant)) = (&family, record.grant) {
            connection.execute("INSERT INTO auth_oauth_families(family_lookup_hash,oauth_grant_id,expires_at,revision) SELECT ?1,oauth_grant_id,expires_at,1 FROM auth_oauth_grants WHERE oauth_grant_id=?2 ON CONFLICT(family_lookup_hash) DO NOTHING", params![family,grant]).map_err(failure)?;
        }
        connection.execute("INSERT INTO auth_oauth_artifacts(lookup_hash,kind,client_id,user_code_hash,grant_established_at_ns,family_lookup_hash,oauth_grant_id,sealed_record,expires_at,revision) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1) ON CONFLICT(kind,lookup_hash) DO UPDATE SET client_id=excluded.client_id,user_code_hash=excluded.user_code_hash,grant_established_at_ns=excluded.grant_established_at_ns,family_lookup_hash=excluded.family_lookup_hash,oauth_grant_id=excluded.oauth_grant_id,sealed_record=excluded.sealed_record,expires_at=excluded.expires_at,revision=revision+1", params![key,record.kind,record.client,record.user_code,nanos(record.established)?,family,record.grant,sealed,record.expires.map(seconds).transpose()?.unwrap_or(i64::MAX)]).map_err(failure)?;
        Ok(())
    }

    fn encode<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        value: &T,
    ) -> Result<Vec<u8>, StorageError> {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(failure)?;
        let mut plaintext = serde_json::to_vec(value).map_err(failure)?;
        let associated = format!("{kind}:{key}");
        self.sealing_key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(associated.as_bytes()),
                &mut plaintext,
            )
            .map_err(|_| StorageError::new("OAuth record sealing failed"))?;
        serde_json::to_vec(&Sealed {
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            ciphertext: URL_SAFE_NO_PAD.encode(plaintext),
        })
        .map_err(failure)
    }

    fn take<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Result<Option<T>, StorageError> {
        self.transaction(|connection| {
            let value = self.read(connection, kind, key)?;
            connection
                .execute(
                    "DELETE FROM auth_oauth_artifacts WHERE kind=?1 AND lookup_hash=?2",
                    params![kind, hash(key)],
                )
                .map_err(failure)?;
            Ok(value)
        })
    }

    fn revoked(
        &self,
        connection: &Connection,
        client: &ClientId,
        family: Option<&str>,
        established: SystemTime,
    ) -> Result<bool, StorageError> {
        connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_barriers WHERE (kind='family' AND scope=?1) OR (kind='client' AND scope=?2 AND recorded_at_ns>=?3)) OR EXISTS(SELECT 1 FROM auth_oauth_families WHERE family_lookup_hash=?1 AND revoked_at IS NOT NULL)", params![family.map(hash),client.as_str(),nanos(established)?], |row| row.get(0)).map_err(failure)
    }

    fn access(&self, connection: &Connection, token: &IssuedToken) -> Result<(), StorageError> {
        self.write(
            connection,
            Record {
                kind: "token",
                key: &token.access_token,
                value: token,
                client: Some(token.client_id.as_str()),
                grant: token.subject.as_deref(),
                family: token.family_id.as_deref(),
                user_code: None,
                established: token.grant_established_at,
                expires: Some(token.expires_at),
            },
        )
    }

    fn refresh(
        &self,
        connection: &Connection,
        token: &RefreshTokenRecord,
    ) -> Result<(), StorageError> {
        self.write(
            connection,
            Record {
                kind: "refresh",
                key: &token.refresh_token,
                value: token,
                client: Some(token.client_id.as_str()),
                grant: token.subject.as_deref(),
                family: Some(&token.family_id),
                user_code: None,
                established: token.grant_established_at,
                expires: token.expires_at,
            },
        )
    }

    fn device(&self, connection: &Connection, grant: &DeviceGrant) -> Result<(), StorageError> {
        self.write(
            connection,
            Record {
                kind: "device",
                key: &grant.device_code,
                value: grant,
                client: Some(grant.client_id.as_str()),
                grant: None,
                family: None,
                user_code: Some(hash(&normalize_user_code(&grant.user_code))),
                established: grant.created_at,
                expires: Some(grant.expires_at),
            },
        )
    }

    fn code(
        &self,
        connection: &Connection,
        code: &AuthorizationCodeRecord,
    ) -> Result<(), StorageError> {
        self.write(
            connection,
            Record {
                kind: "code",
                key: &code.code,
                value: code,
                client: Some(code.client_id.as_str()),
                grant: Some(&code.subject),
                family: None,
                user_code: None,
                established: code.issued_at,
                expires: Some(code.expires_at),
            },
        )
    }

    fn client(&self, connection: &Connection, client: &Client) -> Result<(), StorageError> {
        self.write(
            connection,
            Record {
                kind: "client",
                key: client.client_id.as_str(),
                value: client,
                client: Some(client.client_id.as_str()),
                grant: None,
                family: None,
                user_code: None,
                established: UNIX_EPOCH,
                expires: None,
            },
        )
    }

    fn barrier(
        &self,
        connection: &Connection,
        kind: &str,
        scope: &str,
        window: RevocationWindow,
    ) -> Result<(), StorageError> {
        if scope.is_empty() {
            return Err(StorageError::new("empty OAuth revocation scope"));
        }
        connection.execute("INSERT INTO auth_oauth_barriers VALUES (?1,?2,?3,?4) ON CONFLICT(kind,scope) DO UPDATE SET recorded_at_ns=MAX(recorded_at_ns,excluded.recorded_at_ns),expires_at=MAX(expires_at,excluded.expires_at)", params![kind,scope,nanos(window.recorded_at)?,seconds(window.until)?]).map_err(failure)?;
        Ok(())
    }
}

impl Storage for OAuthStorage {
    async fn get_client(&self, client_id: &ClientId) -> Result<Option<Arc<Client>>, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        let registered: Option<(String, String, String, String)> = connection
            .query_row(
                "SELECT state,kind,display_name,redirect_uris_json FROM auth_oauth_clients WHERE client_id=?1",
                [client_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(failure)?;
        if let Some((state, kind, name, redirects)) = registered {
            if state != "active" {
                return Ok(None);
            }
            let mut grants = vec![
                oauth_as::GrantType::AuthorizationCode,
                oauth_as::GrantType::RefreshToken,
            ];
            if kind == "native" {
                grants.push(oauth_as::GrantType::DeviceCode);
            }
            return Ok(Some(Arc::new(Client {
                client_id: client_id.clone(),
                auth: ClientAuth::Public,
                grant_types: grants,
                redirect_uris: serde_json::from_str(&redirects).map_err(failure)?,
                allowed_scopes: oauth_as::ScopeSet::empty(),
                default_scopes: oauth_as::ScopeSet::empty(),
                name: Some(name),
                registration: None,
            })));
        }
        self.read(&connection, "client", client_id.as_str())
            .map(|client| client.map(Arc::new))
    }

    async fn put_client(&self, client: Client) -> Result<(), StorageError> {
        self.transaction(|connection| {
            let registered: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1)",
                    [client.client_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            if registered {
                return Err(StorageError::new(
                    "registered clients are managed by the administrative Auth API",
                ));
            }
            self.client(connection, &client)
        })
    }

    async fn compare_and_swap_client(
        &self,
        expected: &Client,
        updated: Client,
    ) -> Result<bool, StorageError> {
        self.transaction(|connection| {
            let registered: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1)",
                    [updated.client_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            if registered {
                return Err(StorageError::new(
                    "registered clients are managed by the administrative Auth API",
                ));
            }
            if self
                .read::<Client>(connection, "client", updated.client_id.as_str())?
                .as_ref()
                != Some(expected)
            {
                return Ok(false);
            }
            self.client(connection, &updated)?;
            Ok(true)
        })
    }

    async fn delete_client(
        &self,
        client_id: &ClientId,
        window: RevocationWindow,
    ) -> Result<bool, StorageError> {
        self.transaction(|connection| {
            let registered: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1)",
                    [client_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            if registered {
                return Err(StorageError::new(
                    "registered clients are managed by the administrative Auth API",
                ));
            }
            self.barrier(connection, "client", client_id.as_str(), window)?;
            let existed = self
                .read::<Client>(connection, "client", client_id.as_str())?
                .is_some();
            connection
                .execute(
                    "DELETE FROM auth_oauth_artifacts WHERE client_id=?1",
                    [client_id.as_str()],
                )
                .map_err(failure)?;
            Ok(existed)
        })
    }

    async fn put_device_grant(&self, grant: DeviceGrant) -> Result<(), StorageError> {
        self.transaction(|connection| self.device(connection, &grant))
    }

    async fn get_device_grant(
        &self,
        device_code: &str,
    ) -> Result<Option<DeviceGrant>, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        self.read(&connection, "device", device_code)
    }

    async fn find_device_grant_by_user_code(
        &self,
        normalized_user_code: &str,
    ) -> Result<Option<DeviceGrant>, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        let sealed: Option<(String,Vec<u8>)> = connection.query_row("SELECT lookup_hash,sealed_record FROM auth_oauth_artifacts WHERE kind='device' AND user_code_hash=?1", [hash(normalized_user_code)], |row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(failure)?;
        sealed
            .map(|(key, sealed)| self.decode("device", &key, &sealed))
            .transpose()
    }

    async fn take_device_grant(
        &self,
        device_code: &str,
    ) -> Result<Option<DeviceGrant>, StorageError> {
        self.take("device", device_code)
    }

    async fn compare_and_swap_device_grant(
        &self,
        expected: &DeviceGrantState,
        updated: DeviceGrant,
    ) -> Result<bool, StorageError> {
        self.transaction(|connection| {
            if self
                .read::<DeviceGrant>(connection, "device", &updated.device_code)?
                .as_ref()
                .map(|grant| &grant.state)
                != Some(expected)
            {
                return Ok(false);
            }
            self.device(connection, &updated)?;
            Ok(true)
        })
    }

    async fn put_authorization_code(
        &self,
        code: AuthorizationCodeRecord,
    ) -> Result<(), StorageError> {
        self.transaction(|connection| self.code(connection, &code))
    }

    async fn compare_and_swap_authorization_code(
        &self,
        expected: &AuthorizationCodeState,
        updated: AuthorizationCodeRecord,
    ) -> Result<bool, StorageError> {
        self.transaction(|connection| {
            if self
                .read::<AuthorizationCodeRecord>(connection, "code", &updated.code)?
                .as_ref()
                .map(|code| &code.state)
                != Some(expected)
            {
                return Ok(false);
            }
            self.code(connection, &updated)?;
            Ok(true)
        })
    }

    async fn take_authorization_code(
        &self,
        code: &str,
    ) -> Result<Option<AuthorizationCodeRecord>, StorageError> {
        self.take("code", code)
    }

    async fn put_token(&self, token: IssuedToken) -> Result<WriteOutcome, StorageError> {
        self.transaction(|connection| {
            if self.revoked(
                connection,
                &token.client_id,
                token.family_id.as_deref(),
                token.grant_established_at,
            )? {
                return Ok(WriteOutcome::RefusedRevoked);
            }
            self.access(connection, &token)?;
            Ok(WriteOutcome::Applied)
        })
    }

    async fn get_token(&self, token: &str) -> Result<Option<Arc<IssuedToken>>, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        self.read(&connection, "token", token)
            .map(|record| record.map(Arc::new))
    }

    async fn delete_token(&self, token: &str) -> Result<(), StorageError> {
        self.take::<IssuedToken>("token", token).map(|_| ())
    }

    async fn put_refresh_token(
        &self,
        token: RefreshTokenRecord,
    ) -> Result<WriteOutcome, StorageError> {
        self.transaction(|connection| {
            if self.revoked(
                connection,
                &token.client_id,
                Some(&token.family_id),
                token.grant_established_at,
            )? {
                return Ok(WriteOutcome::RefusedRevoked);
            }
            self.refresh(connection, &token)?;
            Ok(WriteOutcome::Applied)
        })
    }

    async fn get_refresh_token(
        &self,
        token: &str,
    ) -> Result<Option<Arc<RefreshTokenRecord>>, StorageError> {
        let connection = self.authority.connection.lock().map_err(failure)?;
        self.read(&connection, "refresh", token)
            .map(|record| record.map(Arc::new))
    }

    async fn rotate_refresh_token(
        &self,
        expected: &RefreshTokenRecord,
        spent: &RefreshTokenRecord,
        access: &IssuedToken,
        next: Option<&RefreshTokenRecord>,
    ) -> Result<bool, StorageError> {
        self.transaction(|connection| {
            if self
                .read::<RefreshTokenRecord>(connection, "refresh", &expected.refresh_token)?
                .as_ref()
                != Some(expected)
                || self.revoked(
                    connection,
                    &expected.client_id,
                    Some(&expected.family_id),
                    expected.grant_established_at,
                )?
            {
                return Ok(false);
            }
            self.refresh(connection, spent)?;
            self.access(connection, access)?;
            if let Some(next) = next {
                self.refresh(connection, next)?;
            }
            Ok(true)
        })
    }

    async fn take_refresh_token(
        &self,
        token: &str,
    ) -> Result<Option<RefreshTokenRecord>, StorageError> {
        self.take("refresh", token)
    }

    async fn revoke_token_family(
        &self,
        family_id: &str,
        window: RevocationWindow,
    ) -> Result<u64, StorageError> {
        self.transaction(|connection| {
            let family = hash(family_id);
            self.barrier(connection,"family",&family,window)?;
            let grant: Option<String> = connection.query_row("SELECT oauth_grant_id FROM auth_oauth_families WHERE family_lookup_hash=?1", [&family], |row|row.get(0)).optional().map_err(failure)?;
            let now = seconds(window.recorded_at)?;
            connection.execute("UPDATE auth_oauth_families SET revoked_at=coalesce(revoked_at,?2),revision=revision+1 WHERE family_lookup_hash=?1 AND revoked_at IS NULL", params![family,now]).map_err(failure)?;
            if let Some(grant) = grant { enqueue_scope(connection,&EnforcementScope::Grant(grant),now).map_err(failure)?; }
            connection.execute("DELETE FROM auth_oauth_artifacts WHERE family_lookup_hash=?1", [family]).map(|count|count as u64).map_err(failure)
        })
    }

    async fn claim_replay_id(
        &self,
        replay_id: &str,
        expires_at: SystemTime,
    ) -> Result<bool, StorageError> {
        let now = SystemTime::now();
        self.transaction(|connection| {
            let key = hash(replay_id);
            connection.execute("DELETE FROM auth_oauth_artifacts WHERE kind='dpopReplay' AND lookup_hash=?1 AND expires_at<=?2", params![key,seconds(now)?]).map_err(failure)?;
            let present: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_artifacts WHERE kind='dpopReplay' AND lookup_hash=?1)", [&key], |row|row.get(0)).map_err(failure)?;
            if present { return Ok(false); }
            self.write(connection,Record { kind:"dpopReplay",key:replay_id,value:&(),client:None,grant:None,family:None,user_code:None,established:now,expires:Some(expires_at) })?;
            Ok(true)
        })
    }

    async fn sweep_expired(&self, now: SystemTime) -> Result<u64, StorageError> {
        self.transaction(|connection| {
            let now = seconds(now)?;
            let records = connection
                .execute(
                    "DELETE FROM auth_oauth_artifacts WHERE expires_at<=?1",
                    [now],
                )
                .map_err(failure)?;
            let barriers = connection
                .execute(
                    "DELETE FROM auth_oauth_barriers WHERE expires_at<=?1",
                    [now],
                )
                .map_err(failure)?;
            Ok((records + barriers) as u64)
        })
    }
}
