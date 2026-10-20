//! Owner-only native OAuth custody. No service/device credentials are stored here.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::client::RuntimeAuthorizationTrust;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTrust {
    instance_id: String,
    account: String,
    issuer: trellis_protocol::AuthorityIssuerKey,
    rotation_sequence: trellis_protocol::live::U64s,
}

/// Exact selected issuer, runtime, and app identity for one credential handle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeCredentialScope {
    /// Selected HTTPS OAuth origin (explicit loopback development may use HTTP).
    pub issuer: String,
    /// Pinned selected Trellis installation.
    pub instance_id: String,
    /// Pinned public NATS account.
    pub account: String,
    /// Registered public native client identity.
    pub client_id: String,
    /// Independently created ULID; different user credentials use different handles.
    pub credential_handle: ulid::Ulid,
}

/// Identity and rotating credential from a verified OAuth response. Never Debug.
#[derive(Serialize, Deserialize)]
pub struct NativeOAuthGrant {
    /// Verified principal owning this credential.
    pub principal_id: String,
    /// Authoritative OAuth grant identity.
    pub oauth_grant_id: String,
    /// Verified login session identity.
    pub login_session_id: String,
    /// Rotating secret, used only by the native OAuth client's token exchange.
    pub refresh_token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    scope: NativeCredentialScope,
    private_key: String,
    grant: Option<NativeOAuthGrant>,
    recovery_required: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    trust: Option<StoredTrust>,
}

struct CustodyLock(fs::File);

impl Drop for CustodyLock {
    fn drop(&mut self) {
        // Release lease ownership even when a concurrent pre-exec child still
        // holds the same open file description. Closing this FD alone cannot.
        let _ = self.0.unlock();
    }
}

/// Holds the cross-process family lock through read, exchange and durable commit.
/// Private key bytes have no public getter. Drop releases the operating-system lock.
pub struct NativeCredentialLease {
    _lock: CustodyLock,
    path: PathBuf,
    record: Record,
    signing_key: SigningKey,
}

impl std::fmt::Debug for NativeCredentialLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeCredentialLease")
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
fn private_metadata(metadata: &fs::Metadata, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no arguments, borrowed memory, or failure result.
    let owner = unsafe { libc::geteuid() };
    if metadata.uid() != owner
        || metadata.mode() & 0o077 != 0
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OAuth custody must be owner-only",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path, create: bool) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    private_metadata(&file.metadata()?, false)?;
    Ok(file)
}

impl NativeCredentialLease {
    pub(crate) fn restore_trust(
        &mut self,
        initial: &RuntimeAuthorizationTrust,
    ) -> io::Result<RuntimeAuthorizationTrust> {
        if self.record.trust.is_none() {
            if self.record.grant.is_some() || self.record.recovery_required {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "credential trust missing",
                ));
            }
            self.retain_trust(initial)?;
        }
        let stored = self
            .record
            .trust
            .as_ref()
            .ok_or_else(|| io::Error::other("credential trust missing"))?;
        if stored.instance_id != self.record.scope.instance_id
            || stored.account != self.record.scope.account
            || stored.issuer.state != trellis_protocol::AuthorityIssuerState::Active
            || stored.issuer.verifying_key().is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid credential trust",
            ));
        }
        Ok(RuntimeAuthorizationTrust {
            instance_id: stored.instance_id.clone(),
            account: stored.account.clone(),
            issuer: stored.issuer.clone(),
            rotation_sequence: stored.rotation_sequence.get(),
        })
    }

    /// Persist only trust authenticated by the connection owner or pinned issuer chain.
    /// The same record holds both the rotation floor and token-spent marker.
    pub(crate) fn retain_trust(&mut self, trust: &RuntimeAuthorizationTrust) -> io::Result<()> {
        if trust.instance_id != self.record.scope.instance_id
            || trust.account != self.record.scope.account
            || trust.issuer.state != trellis_protocol::AuthorityIssuerState::Active
            || trust.issuer.verifying_key().is_err()
            || self.record.trust.as_ref().is_some_and(|stored| {
                trust.rotation_sequence < stored.rotation_sequence.get()
                    || (trust.rotation_sequence == stored.rotation_sequence.get()
                        && trust.issuer != stored.issuer)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid credential trust replacement",
            ));
        }
        self.record.trust = Some(StoredTrust {
            instance_id: trust.instance_id.clone(),
            account: trust.account.clone(),
            issuer: trust.issuer.clone(),
            rotation_sequence: trellis_protocol::live::U64s::new(trust.rotation_sequence),
        });
        self.commit()
    }

    /// Selected identity for this locked credential; contains no secret material.
    pub fn scope(&self) -> &NativeCredentialScope {
        &self.record.scope
    }

    /// Open or create owner-only custody and acquire its exclusive family lock.
    /// This blocking filesystem boundary belongs on the client's blocking executor.
    /// Unsupported platforms require an explicitly supplied secure-store adapter.
    pub fn lock(directory: &Path, scope: NativeCredentialScope) -> io::Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = (directory, scope);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "an owner-only OAuth store adapter is required on this platform",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            if !directory.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "OAuth custody path must be absolute",
                ));
            }
            let issuer = url::Url::parse(&scope.issuer).map_err(io::Error::other)?;
            let loopback = match issuer.host() {
                Some(url::Host::Domain("localhost")) => true,
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            };
            if issuer.origin().ascii_serialization() != scope.issuer
                || !(issuer.scheme() == "https" || (issuer.scheme() == "http" && loopback))
                || [&scope.instance_id, &scope.account, &scope.client_id]
                    .iter()
                    .any(|value| value.is_empty() || value.len() > 4096)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid OAuth custody scope",
                ));
            }
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
            private_metadata(&fs::symlink_metadata(directory)?, true)?;
            // A writable non-sticky ancestor could let another user replace the
            // entire credential directory despite its private leaf permissions.
            // SAFETY: geteuid has no arguments, memory access, or failure result.
            let owner = unsafe { libc::geteuid() };
            for parent in directory.ancestors().skip(1) {
                let metadata = fs::symlink_metadata(parent)?;
                if !metadata.is_dir()
                    || (metadata.uid() != owner && metadata.uid() != 0)
                    || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unsafe OAuth custody ancestor",
                    ));
                }
            }
            let identity = URL_SAFE_NO_PAD.encode(Sha256::digest(
                serde_json::to_vec(&scope).map_err(io::Error::other)?,
            ));
            let lock = private_file(&directory.join(format!("{identity}.lock")), true)?;
            lock.lock()?;
            let lock = CustodyLock(lock);
            let path = directory.join(format!("{identity}.json"));
            let record = match private_file(&path, false) {
                Ok(file) => {
                    if file.metadata()?.len() > 64 * 1024 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "oversized OAuth credential",
                        ));
                    }
                    let mut bytes = Vec::new();
                    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                    if bytes.len() > 64 * 1024 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "oversized OAuth credential",
                        ));
                    }
                    let record: Record = serde_json::from_slice(&bytes).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid OAuth credential")
                    })?;
                    if serde_json::to_vec(&record.scope).map_err(io::Error::other)?
                        != serde_json::to_vec(&scope).map_err(io::Error::other)?
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "OAuth credential scope mismatch",
                        ));
                    }
                    record
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let signing_key = SigningKey::from_slice(&rand::random::<[u8; 32]>())
                        .map_err(|_| io::Error::other("cannot generate DPoP key"))?;
                    Record {
                        scope,
                        private_key: URL_SAFE_NO_PAD.encode(signing_key.to_bytes()),
                        grant: None,
                        recovery_required: false,
                        trust: None,
                    }
                }
                Err(error) => return Err(error),
            };
            let bytes = URL_SAFE_NO_PAD.decode(&record.private_key).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid DPoP credential")
            })?;
            let signing_key = SigningKey::from_slice(&bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid DPoP credential")
            })?;
            let store = Self {
                _lock: lock,
                path,
                record,
                signing_key,
            };
            store.commit()?;
            Ok(store)
        }
    }

    /// Public RFC 7517 P-256 JWK for code binding and standards DPoP proofs.
    pub fn public_jwk(&self) -> serde_json::Value {
        let point = self.signing_key.verifying_key().to_encoded_point(false);
        let bytes = point.as_bytes();
        serde_json::json!({"kty":"EC","crv":"P-256","x":URL_SAFE_NO_PAD.encode(&bytes[1..33]),"y":URL_SAFE_NO_PAD.encode(&bytes[33..65])})
    }

    /// Sign the standards client's exact JWS signing input without exporting a key.
    pub fn sign_dpop(&self, signing_input: &[u8]) -> [u8; 64] {
        let signature: Signature = self.signing_key.sign(signing_input);
        signature.to_bytes().into()
    }

    /// Persist a successful verified authorization or refresh response atomically.
    pub fn install(&mut self, grant: NativeOAuthGrant) -> io::Result<()> {
        if grant.refresh_token.is_empty()
            || grant.refresh_token.len() > 16 * 1024
            || [
                &grant.principal_id,
                &grant.oauth_grant_id,
                &grant.login_session_id,
            ]
            .iter()
            .any(|value| value.is_empty() || value.len() > 4096)
            || self
                .record
                .grant
                .as_ref()
                .is_some_and(|old| old.principal_id != grant.principal_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OAuth credential identity mismatch",
            ));
        }
        self.record.grant = Some(grant);
        self.record.recovery_required = false;
        if let Err(error) = self.commit() {
            self.record.recovery_required = true;
            return Err(error);
        }
        Ok(())
    }

    /// Mark the family before network I/O. Response loss requires standard OAuth
    /// recovery; reopening this file can never silently retry the consumed token.
    pub fn begin_rotation(&mut self) -> io::Result<String> {
        if self.record.recovery_required {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "OAuth recovery required",
            ));
        }
        let token = self
            .record
            .grant
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OAuth login required"))?
            .refresh_token
            .clone();
        self.record.recovery_required = true;
        self.commit()?;
        Ok(token)
    }

    /// Retain a known, unsubmitted successor secret without trusting response identity.
    /// The exclusive lease and existing grant identity remain unchanged while an
    /// admission awaits issuer-chain verification. An ambiguous exchange cannot
    /// call this method or make its possibly consumed token reusable.
    pub(crate) fn retain_rotated_token(&mut self, token: String) -> io::Result<()> {
        let grant = self
            .record
            .grant
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OAuth login required"))?;
        if !self.record.recovery_required
            || token.is_empty()
            || token.len() > 16 * 1024
            || token == grant.refresh_token
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid OAuth refresh successor",
            ));
        }
        grant.refresh_token = token;
        self.record.recovery_required = false;
        if let Err(error) = self.commit() {
            self.record.recovery_required = true;
            return Err(error);
        }
        Ok(())
    }

    fn commit(&self) -> io::Result<()> {
        #[cfg(not(unix))]
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "owner-only store unavailable",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let staged = self
                .path
                .with_extension(format!("{}.tmp", ulid::Ulid::new()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&staged)?;
            let result = (|| {
                let bytes = serde_json::to_vec(&self.record).map_err(io::Error::other)?;
                if bytes.len() > 64 * 1024 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "oversized OAuth credential",
                    ));
                }
                file.write_all(&bytes)?;
                file.sync_all()?;
                fs::rename(&staged, &self.path)?;
                fs::File::open(
                    self.path
                        .parent()
                        .ok_or_else(|| io::Error::other("credential parent missing"))?,
                )?
                .sync_all()
            })();
            if result.is_err() {
                let _ = fs::remove_file(&staged);
            }
            result
        }
    }
}

#[cfg(all(test, unix))]
#[path = "oauth_store_tests.rs"]
mod tests;
