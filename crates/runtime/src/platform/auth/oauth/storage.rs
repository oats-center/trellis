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
use super::super::{AuthError, EnforcementScope, SqliteAuthorizationStore};

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
        self.transaction(|connection| self.cleanup_expired_in(connection, now))
    }

    fn cleanup_expired_in(&self, connection: &Connection, now: i64) -> Result<usize, StorageError> {
        // Pending decisions keep the exact reviewed client, not an unlimited
        // freshness extension for new requests. Approved grants keep only the
        // client identity and protocol fields their remaining lifecycle needs.
        let clients = {
            let mut statement = connection.prepare(
                "SELECT c.client_id,NOT EXISTS(SELECT 1 FROM auth_oauth_clients r WHERE r.client_id=c.client_id) AND EXISTS(SELECT 1 FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) WHERE g.client_id=c.client_id AND g.revoked_at IS NULL AND g.expires_at>?1 AND l.revoked_at IS NULL AND l.expires_at>?1) FROM auth_cimd_clients c WHERE c.expires_at<=?1 AND NOT EXISTS(SELECT 1 FROM auth_oauth_artifacts a WHERE a.client_id=c.client_id AND a.kind IN ('authorization','upstream','device','deviceSelection','code') AND a.expires_at>?1) AND (EXISTS(SELECT 1 FROM auth_oauth_clients r WHERE r.client_id=c.client_id) OR NOT EXISTS(SELECT 1 FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) WHERE g.client_id=c.client_id AND g.revoked_at IS NULL AND g.expires_at>?1 AND l.revoked_at IS NULL AND l.expires_at>?1) OR c.metadata_json!=json_object('application_type',CASE c.kind WHEN 'browser' THEN 'web' ELSE 'native' END,'redirect_uris',json_extract(c.metadata_json,'$.redirect_uris'))) ORDER BY c.expires_at,c.client_id LIMIT 128"
            ).map_err(failure)?;
            let rows = statement
                .query_map([now], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                })
                .map_err(failure)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(failure)?
        };
        let mut removed = 0;
        for (client_id, approved) in clients {
            if approved {
                let mut client: Client = self
                    .read(connection, "client", &client_id)?
                    .ok_or_else(|| StorageError::new("approved client artifact missing"))?;
                client.name = None;
                let sealed = self.encode("client", &hash(&client_id), &client)?;
                connection.execute("UPDATE auth_oauth_artifacts SET sealed_record=?2,revision=revision+1 WHERE kind='client' AND lookup_hash=?1",params![hash(&client_id),sealed]).map_err(failure)?;
                connection.execute("UPDATE auth_cimd_clients SET metadata_json=json_object('application_type',CASE kind WHEN 'browser' THEN 'web' ELSE 'native' END,'redirect_uris',json_extract(metadata_json,'$.redirect_uris')) WHERE client_id=?1",[client_id]).map_err(failure)?;
            } else {
                removed += connection
                    .execute(
                        "DELETE FROM auth_oauth_artifacts WHERE kind='client' AND client_id=?1",
                        [&client_id],
                    )
                    .map_err(failure)?;
                removed += connection
                    .execute(
                        "DELETE FROM auth_cimd_clients WHERE client_id=?1",
                        [client_id],
                    )
                    .map_err(failure)?;
            }
        }
        removed+=connection.execute(
                "DELETE FROM auth_oauth_artifacts WHERE rowid IN (SELECT a.rowid FROM auth_oauth_artifacts a WHERE a.kind!='client' AND a.expires_at<=?1 AND (a.kind!='refresh' OR NOT EXISTS(SELECT 1 FROM auth_oauth_families f WHERE f.family_lookup_hash=a.family_lookup_hash AND f.expires_at>?1)) ORDER BY a.expires_at,a.rowid LIMIT 256)",
                [now],
            ).map_err(failure)?;
        Ok(removed)
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
    ) -> Result<(), AuthError> {
        let client = document.document.to_client();
        let observed = seconds(document.observed_at).map_err(|_| AuthError::Unavailable)?;
        let expires = observed.checked_add(300).ok_or(AuthError::Unavailable)?;
        let metadata = serde_json::to_string(&document.metadata)?;
        let sealed = self
            .encode("client", &hash(client.client_id.as_str()), &client)
            .map_err(|_| AuthError::Unavailable)?;
        let now = seconds(SystemTime::now()).map_err(|_| AuthError::Unavailable)?;
        if expires <= now {
            return Err(AuthError::Denied);
        }
        self.authority.transaction(|connection| {
            let registered: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1)", [client.client_id.as_str()], |row|row.get(0))?;
            if registered {return Err(AuthError::Conflict);}
            self.cleanup_expired_in(connection,now).map_err(|_| AuthError::Unavailable)?;
            let pending_changed:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_cimd_clients c WHERE c.client_id=?1 AND c.metadata_digest!=?2 AND EXISTS(SELECT 1 FROM auth_oauth_artifacts a WHERE a.client_id=c.client_id AND a.kind IN ('authorization','upstream','device','deviceSelection','code') AND a.expires_at>?3))",params![client.client_id.as_str(),document.revision,now],|row|row.get(0))?;
            if pending_changed {return Err(AuthError::Conflict);}
            let approved:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) WHERE g.client_id=?1 AND g.revoked_at IS NULL AND g.expires_at>?2 AND l.revoked_at IS NULL AND l.expires_at>?2)",params![client.client_id.as_str(),now],|row|row.get(0))?;
            if !approved {
                let (count,bytes):(i64,i64)=connection.query_row("SELECT count(*),coalesce(sum(length(CAST(c.metadata_json AS BLOB))+coalesce(length(a.sealed_record),0)),0) FROM auth_cimd_clients c LEFT JOIN auth_oauth_artifacts a ON a.client_id=c.client_id AND a.kind='client' WHERE c.client_id!=?1 AND NOT EXISTS(SELECT 1 FROM auth_oauth_clients r WHERE r.client_id=c.client_id) AND NOT EXISTS(SELECT 1 FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) WHERE g.client_id=c.client_id AND g.revoked_at IS NULL AND g.expires_at>?2 AND l.revoked_at IS NULL AND l.expires_at>?2)",params![client.client_id.as_str(),now],|row|Ok((row.get(0)?,row.get(1)?)))?;
                if count>=128 || bytes.saturating_add(metadata.len() as i64).saturating_add(sealed.len() as i64)>16*1024*1024 {return Err(AuthError::ResourceLimit);}
            }
            connection.execute("INSERT INTO auth_oauth_artifacts(lookup_hash,kind,client_id,grant_established_at_ns,sealed_record,expires_at,revision) VALUES(?1,'client',?2,?3,?4,?5,1) ON CONFLICT(kind,lookup_hash) DO UPDATE SET sealed_record=excluded.sealed_record,expires_at=excluded.expires_at,revision=revision+1",params![hash(client.client_id.as_str()),client.client_id.as_str(),nanos(document.observed_at).map_err(|_|AuthError::Unavailable)?,sealed,expires])?;
            connection.execute("INSERT INTO auth_cimd_clients(client_id,kind,metadata_digest,metadata_json,observed_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(client_id) DO UPDATE SET kind=excluded.kind,metadata_digest=excluded.metadata_digest,metadata_json=excluded.metadata_json,observed_at=excluded.observed_at,expires_at=excluded.expires_at",params![client.client_id.as_str(),document.kind,document.revision,metadata,observed,expires])?;
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
        let mut flow: super::portal::PortalAuthorization = self
            .read(&connection, "authorization", id)?
            .ok_or_else(|| StorageError::new("authorization unavailable"))?;
        if flow.session_id != session || flow.expires_at <= now {
            return Err(StorageError::new("authorization unavailable"));
        }
        if let Some(presentation) = &mut flow.presentation {
            let lookup = hash(id);
            let sealed: Vec<u8> = connection.query_row("SELECT sealed_record FROM auth_oauth_artifacts WHERE kind='authorization' AND lookup_hash=?1",[&lookup],|row|row.get(0)).map_err(failure)?;
            presentation.lookup_hash = lookup;
            presentation.sealed_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(&sealed));
        }
        Ok(flow)
    }

    /// Save the displayed meaning in the same transaction that reads its policy.
    /// Updating an existing artifact cannot resurrect an already consumed flow.
    pub(super) fn present_authorization(
        &self,
        connection: &Connection,
        id: &str,
        expected: &super::portal::PortalAuthorization,
        presentation: super::portal::ConsentPresentation,
        now: i64,
    ) -> Result<(), StorageError> {
        let mut current: super::portal::PortalAuthorization = self
            .read(connection, "authorization", id)?
            .ok_or_else(|| StorageError::new("authorization unavailable"))?;
        if current.session_id != expected.session_id
            || current.expires_at <= now
            || current.public_jwk != expected.public_jwk
            || serde_json::to_value(&current.request).map_err(failure)?
                != serde_json::to_value(&expected.request).map_err(failure)?
        {
            return Err(StorageError::new("authorization unavailable"));
        }
        current.presentation = Some(presentation);
        let lookup = hash(id);
        let sealed = self.encode("authorization", &lookup, &current)?;
        connection.execute("UPDATE auth_oauth_artifacts SET sealed_record=?1,revision=revision+1 WHERE kind='authorization' AND lookup_hash=?2",params![sealed,lookup]).map_err(failure)?;
        Ok(())
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

    pub(super) fn decode<T: DeserializeOwned>(
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
        if record.kind == "refresh" {
            let family = family
                .as_ref()
                .ok_or_else(|| StorageError::new("refresh family missing"))?;
            let grant = record
                .grant
                .ok_or_else(|| StorageError::new("refresh grant missing"))?;
            let expires = record
                .expires
                .map(seconds)
                .transpose()?
                .ok_or_else(|| StorageError::new("absolute refresh expiry missing"))?;
            // Only the engine's initial refresh artifact establishes the absolute
            // family deadline. Access-token expiry is unrelated, and rotations
            // must never slide the original deadline.
            connection.execute("INSERT INTO auth_oauth_families(family_lookup_hash,oauth_grant_id,expires_at,revision) SELECT ?1,g.oauth_grant_id,min(?3,g.expires_at,l.expires_at),1 FROM auth_oauth_grants g JOIN auth_login_sessions l ON l.login_session_id=g.login_session_id WHERE g.oauth_grant_id=?2 ON CONFLICT(family_lookup_hash) DO NOTHING", params![family,grant,expires]).map_err(failure)?;
        }
        connection.execute("INSERT INTO auth_oauth_artifacts(lookup_hash,kind,client_id,user_code_hash,grant_established_at_ns,family_lookup_hash,oauth_grant_id,sealed_record,expires_at,revision) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1) ON CONFLICT(kind,lookup_hash) DO UPDATE SET client_id=excluded.client_id,user_code_hash=excluded.user_code_hash,grant_established_at_ns=excluded.grant_established_at_ns,family_lookup_hash=excluded.family_lookup_hash,oauth_grant_id=excluded.oauth_grant_id,sealed_record=excluded.sealed_record,expires_at=excluded.expires_at,revision=revision+1", params![key,record.kind,record.client,record.user_code,nanos(record.established)?,family,record.grant,sealed,record.expires.map(seconds).transpose()?.unwrap_or(i64::MAX)]).map_err(failure)?;
        Ok(())
    }

    pub(super) fn encode<T: Serialize>(
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
        connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_barriers WHERE (kind='family' AND scope=?1) OR (kind='client' AND scope=?2 AND recorded_at_ns>=?3)) OR EXISTS(SELECT 1 FROM auth_oauth_families WHERE family_lookup_hash=?1 AND (revoked_at IS NOT NULL OR expires_at<=?4))", params![family.map(hash),client.as_str(),nanos(established)?,seconds(SystemTime::now())?], |row| row.get(0)).map_err(failure)
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
        let registered: Option<(String, String, String, String, String)> = connection
            .query_row(
                "SELECT state,kind,display_name,redirect_uris_json,metadata_json FROM auth_oauth_clients WHERE client_id=?1",
                [client_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(failure)?;
        if let Some((state, kind, name, redirects, metadata)) = registered {
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
            let metadata: serde_json::Value = serde_json::from_str(&metadata).map_err(failure)?;
            if let Some(configured) = metadata.get("grant_types") {
                grants = configured
                    .as_array()
                    .ok_or_else(|| StorageError::new("invalid registered grant types"))?
                    .iter()
                    .map(|grant| match grant.as_str() {
                        Some("authorization_code") => Ok(oauth_as::GrantType::AuthorizationCode),
                        Some("refresh_token") => Ok(oauth_as::GrantType::RefreshToken),
                        Some("urn:ietf:params:oauth:grant-type:device_code") => {
                            Ok(oauth_as::GrantType::DeviceCode)
                        }
                        _ => Err(StorageError::new("invalid registered grant type")),
                    })
                    .collect::<Result<_, _>>()?;
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
        let now = seconds(SystemTime::now())?;
        let retained:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_cimd_clients c WHERE c.client_id=?1 AND (c.expires_at>?2 OR EXISTS(SELECT 1 FROM auth_oauth_artifacts a WHERE a.client_id=c.client_id AND a.kind IN ('authorization','upstream','device','deviceSelection','code') AND a.expires_at>?2) OR EXISTS(SELECT 1 FROM auth_oauth_grants g JOIN auth_login_sessions l USING(login_session_id) WHERE g.client_id=c.client_id AND g.revoked_at IS NULL AND g.expires_at>?2 AND l.revoked_at IS NULL AND l.expires_at>?2)))",params![client_id.as_str(),now],|row|row.get(0)).map_err(failure)?;
        if !retained {
            return Ok(None);
        }
        self.read(&connection, "client", client_id.as_str())
            .map(|client| client.map(Arc::new))
    }

    async fn put_client(&self, _client: Client) -> Result<(), StorageError> {
        // Trellis does not expose the engine's dynamic registration paths. CIMD
        // metadata and its bounded artifact must be installed atomically.
        Err(StorageError::new(
            "clients are managed by Auth registration or validated CIMD intake",
        ))
    }

    async fn compare_and_swap_client(
        &self,
        _expected: &Client,
        _updated: Client,
    ) -> Result<bool, StorageError> {
        Err(StorageError::new(
            "clients are managed by Auth registration or validated CIMD intake",
        ))
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
            connection
                .execute(
                    "DELETE FROM auth_cimd_clients WHERE client_id=?1",
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
            let records = self.cleanup_expired_in(connection, now)?;
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

#[cfg(test)]
mod tests {
    use super::super::portal::{ConsentRequest, PortalAuthorization};
    use super::*;
    use oauth_as::{
        authorization::AuthorizationRequest,
        cimd::{CimdPolicy, ClientIdUrl, ValidatedClientIdDocument},
        jwt::{EcdsaP256Key, JwsSigner},
    };

    #[tokio::test]
    async fn live_consent_transactions_pin_client_capacity_until_their_own_expiry() {
        let directory = tempfile::tempdir().unwrap();
        let mut connection = Connection::open(directory.path().join("platform.db")).unwrap();
        let migration = refinery::Migration::unapplied(
            "V1000__platform_init",
            include_str!("../../../storage/sqlite/platform/V1000__platform_init.sql"),
        )
        .unwrap();
        refinery::Runner::new(&[migration])
            .run(&mut connection)
            .unwrap();
        let authority = Arc::new(
            SqliteAuthorizationStore::from_connection(
                connection,
                ed25519_dalek::SigningKey::from_bytes(&[21; 32]),
                super::super::super::sqlite::AuthSettings {
                    instance_id: ulid::Ulid::new().to_string(),
                    account: "cimd-retention".into(),
                    oauth_origin: "https://auth.example".into(),
                    upstream_tls_roots_pem: Vec::new(),
                    authority_lifetime_seconds: 300,
                    clock_skew_seconds: 30,
                    login_lifetime_seconds: 86400,
                    enforcement_page_size: 128,
                },
            )
            .unwrap(),
        );
        let now = seconds(SystemTime::now()).unwrap();
        let guest = authority.create_portal_session(now).unwrap();
        let engine =
            super::super::OAuthEngine::new(OAuthStorage::new(authority, &[17; 32]).unwrap())
                .unwrap();
        let key = EcdsaP256Key::generate("pending-cimd");
        let document = |client_id: &str, name: &str| {
            let metadata = serde_json::json!({"client_id":client_id,"application_type":"native","client_name":name,"redirect_uris":["http://127.0.0.1:9123/callback"],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]});
            let body = serde_json::to_vec(&metadata).unwrap();
            let mut policy = CimdPolicy::new();
            policy.redirect_uris_same_origin = false;
            let identity = ClientIdUrl::parse(client_id, &policy).unwrap();
            super::super::ClientDocument {
                document: ValidatedClientIdDocument::validate(&identity, &body, &policy).unwrap(),
                revision: URL_SAFE_NO_PAD.encode(Sha256::digest(&body)),
                metadata,
                kind: "native".into(),
                observed_at: SystemTime::now(),
            }
        };
        let mut transactions = Vec::new();
        let mut clients = Vec::new();
        for _ in 0..128 {
            let client_id = format!(
                "https://{}.example/client.json",
                ulid::Ulid::new().to_string().to_ascii_lowercase()
            );
            engine
                .server
                .store()
                .install_client_document(&document(&client_id, "Reviewed client"))
                .unwrap();
            let pairs=vec![
                ("response_type".into(),"code".into()),("client_id".into(),client_id.clone()),("redirect_uri".into(),"http://127.0.0.1:9123/callback".into()),
                ("code_challenge".into(),URL_SAFE_NO_PAD.encode(Sha256::digest(b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"))),("code_challenge_method".into(),"S256".into()),
                ("dpop_jkt".into(),key.public_jwk().thumbprint()),("authorization_details".into(),serde_json::json!([{"type":"trellis_capabilities","required":[],"optional":[]}]).to_string()),
            ];
            let request = AuthorizationRequest::from_pairs(
                pairs
                    .iter()
                    .map(|(key, value): &(String, String)| (key.as_str(), value.as_str())),
            );
            engine
                .server
                .validate_authorization_request(&request)
                .await
                .unwrap();
            let transaction = ulid::Ulid::new().to_string();
            engine
                .server
                .store()
                .store_authorization(
                    &transaction,
                    &PortalAuthorization {
                        session_id: guest.session_id.clone(),
                        request: ConsentRequest::Authorization(pairs),
                        public_jwk: serde_json::to_value(key.public_jwk()).unwrap(),
                        expires_at: now + 600,
                        presentation: None,
                    },
                    &client_id,
                )
                .unwrap();
            transactions.push(transaction);
            clients.push(ClientId::new(client_id));
        }
        engine.server.store().cleanup_expired(now + 301).unwrap();
        for (transaction, client) in transactions.iter().zip(&clients) {
            assert!(engine
                .server
                .store()
                .authorization(transaction, &guest.session_id, now + 301)
                .is_ok());
            assert!(engine
                .server
                .store()
                .get_client(client)
                .await
                .unwrap()
                .is_some());
        }
        assert!(matches!(
            engine
                .server
                .store()
                .install_client_document(&document(clients[0].as_str(), "Changed client")),
            Err(AuthError::Conflict)
        ));
        let overflow = format!(
            "https://{}.example/client.json",
            ulid::Ulid::new().to_string().to_ascii_lowercase()
        );
        assert!(matches!(
            engine
                .server
                .store()
                .install_client_document(&document(&overflow, "Overflow client")),
            Err(AuthError::ResourceLimit)
        ));
        engine
            .server
            .store()
            .sweep_expired(UNIX_EPOCH + std::time::Duration::from_secs((now + 601) as u64))
            .await
            .unwrap();
        for (transaction, client) in transactions.iter().zip(&clients) {
            assert!(engine
                .server
                .store()
                .authorization(transaction, &guest.session_id, now + 601)
                .is_err());
            assert!(engine
                .server
                .store()
                .get_client(client)
                .await
                .unwrap()
                .is_none());
        }
        engine
            .server
            .store()
            .install_client_document(&document(&overflow, "Fresh client"))
            .unwrap();
        assert!(engine
            .server
            .store()
            .get_client(&ClientId::new(overflow))
            .await
            .unwrap()
            .is_some());
    }
}
