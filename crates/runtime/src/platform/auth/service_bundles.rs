//! Moving one service deployment to another installation.
//!
//! See `design/auth/service-export.md`. `Deployments.Export` reads a service
//! deployment, its installed build, instances, enrollment secrets, settings
//! with their secrets opened, webhook endpoints, and the data of its KV
//! buckets, stores and unfinished jobs into one bundle, and changes nothing.
//! The bundle waits, encrypted with a key held only in memory, until its
//! exporter downloads it with `Deployments.Exports.Download`. The CLI
//! encrypts it with the operator's passphrase as it arrives, so the
//! passphrase never reaches Auth.
//!
//! `Deployments.Import` receives the same content as its byte stream, spools
//! it encrypted the same way, and checks every part against the manifest
//! before it changes anything. It then commits the deployment under its own
//! ID, with its instances' public keys, its settings sealed with this
//! installation's storage key, and its resources recorded as pending, and
//! afterwards loads their data. Running the same bundle again finishes an
//! import that stopped while loading.
//!
//! The content is a sequence of named entries, each sent as length-prefixed
//! chunks so that a stream snapshot of unknown size can be written as it is
//! read; `manifest.json` comes last and names every other entry with its size
//! and SHA-256.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::{Arc, Mutex};

use async_nats::jetstream::{context::GetStreamErrorKind, ErrorCode};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use futures_util::StreamExt as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::rand::{SecureRandom as _, SystemRandom};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use trellis_rs::generated::{ParticipantDescriptor, SerializableErrorData, TrellisError};
use trellis_rs::service::{FileTransferInfo, OperationControl, RequestContext, ServerError};
use trellis_runtime_apis::apis::trellis_auth_v1::{errors, operations, rpc, Provider};

use super::config::BundledSettings;
use super::rpc::{caller, rpc_error};
use super::sqlite::{decode, digest, id, json, Connection};
use super::{
    blocking, Administration, AuthError, Mutation, OAuthEngine, OAuthStorage,
    SqliteAuthorizationStore,
};
use crate::backup::{restore_stream_from, snapshot_stream_into};

/// The stream metadata key that names the import which restored a stream.
const IMPORT_MARKER: &str = "trellis.import";

/// The content format this build writes and reads.
const FORMAT: u32 = 1;
/// The first bytes of the content, before the format number.
const MAGIC: &[u8; 16] = b"TRELLIS-SERVICE\n";
/// Bytes per spool chunk and per content chunk this build writes.
const CHUNK: usize = 64 * 1024;
/// The largest content chunk accepted.
const MAX_CHUNK: usize = 1 << 20;
/// The largest JSON entry an import holds in memory. Only the fixed set of
/// documents is accepted, so this bounds them all together too.
const MAX_DOCUMENT: usize = 16 << 20;
/// The most entries a bundle may have: its documents, its jobs, and one
/// snapshot per KV bucket or store.
const MAX_ENTRIES: usize = 1_024;
/// The longest line of `jobs.jsonl`: one lifecycle message, which the broker
/// limits to its largest payload, in base64 with its headers.
const MAX_JOB_LINE: usize = 8 << 20;
/// How long a finished export waits for its download, in seconds.
const EXPORT_LIFETIME: i64 = 3_600;
/// Job lifecycle events after which a run does nothing more.
const SETTLED: [&str; 8] = [
    "completed",
    "failed",
    "cancelled",
    "expired",
    "skipped",
    "stale",
    "dead",
    "dismissed",
];

/// Register the export and import Operations and the export download.
pub(super) fn register<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    engine: Arc<OAuthEngine>,
    operational: async_nats::Client,
) {
    let directory = engine
        .server
        .store()
        .authority
        .custody_directory
        .with_extension("bundles");
    // Spools hold keys only in memory, so those left by an earlier run can
    // never be read again.
    let _ = std::fs::remove_dir_all(&directory);
    let exports = Arc::new(Exports {
        directory,
        finished: Mutex::default(),
    });
    let (export_engine, export_nats, export_spools) = (
        Arc::clone(&engine),
        operational.clone(),
        Arc::clone(&exports),
    );
    let _ =
        provider.register_deployments_export(move |context, input, operation| {
            let (engine, nats, exports) = (
                Arc::clone(&export_engine),
                export_nats.clone(),
                Arc::clone(&export_spools),
            );
            async move {
                let result = export(&engine, &nats, &exports, &context, &input, &operation).await;
                match result {
                    Ok(output) => operation.complete(output).await.map(|_| ()),
                    Err(error) => operation
                        .fail(
                            failure(&error, |kind, data| match kind {
                                Kind::Auth => operations::DeploymentsExportError::AuthError(
                                    errors::AuthError { error: data },
                                ),
                                Kind::Validation => {
                                    operations::DeploymentsExportError::ValidationError(
                                        errors::ValidationError { error: data },
                                    )
                                }
                                Kind::Unexpected => {
                                    operations::DeploymentsExportError::UnexpectedError(
                                        errors::UnexpectedError { error: data },
                                    )
                                }
                            })
                            .into_provider_failure(),
                        )
                        .await
                        .map(|_| ()),
                }
            }
        });
    let download_exports = Arc::clone(&exports);
    provider.register_deployments_exports_download(move |context, input| {
        let exports = Arc::clone(&download_exports);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            let export_id = input.export_id.to_string();
            let now = crate::clock::unix_now();
            let (spool, size, content_digest) =
                exports.finished(&export_id, &actor, now).ok_or_else(|| {
                    rpc_error(AuthError::NotFound(format!(
                        "no finished export {export_id}; exports wait an hour for their download"
                    )))
                })?;
            let format = &time::format_description::well_known::Rfc3339;
            let at = |seconds: i64| {
                time::OffsetDateTime::from_unix_timestamp(seconds)
                    .ok()
                    .and_then(|at| at.format(format).ok())
                    .unwrap_or_default()
            };
            let plan = context.plan_stream_download_transfer(
                &trellis_rs::service::generate_transfer_id()
                    .map_err(|error| ServerError::Internal(error.to_string()))?,
                &at(now + 600),
                FileTransferInfo {
                    key: format!("{export_id}.content"),
                    size,
                    updated_at: at(now),
                    digest: format!("SHA-256={content_digest}"),
                    content_type: Some("application/octet-stream".into()),
                    metadata: BTreeMap::new(),
                },
            )?;
            let grant = plan.grant.clone();
            context
                .handle()
                .spawn_stream_download_endpoint(plan, spool_reader(spool))
                .await?;
            Ok(rpc::DeploymentsExportsDownloadOutput {
                response: serde_json::from_value(
                    json!({ "exportId": export_id, "size": size.to_string() }),
                )
                .map_err(|error| ServerError::Internal(error.to_string()))?,
                transfer: Some(grant),
            })
        }
    });
    let (import_engine, import_nats, import_exports) = (engine, operational, exports);
    let _ =
        provider.register_deployments_import(move |context, input, operation| {
            let (engine, nats, exports) = (
                Arc::clone(&import_engine),
                import_nats.clone(),
                Arc::clone(&import_exports),
            );
            async move {
                let result = import(
                    &engine,
                    &nats,
                    &exports.directory,
                    &context,
                    &input,
                    &operation,
                )
                .await;
                match result {
                    Ok(output) => operation.complete(output).await.map(|_| ()),
                    Err(error) => operation
                        .fail(
                            failure(&error, |kind, data| match kind {
                                Kind::Auth => operations::DeploymentsImportError::AuthError(
                                    errors::AuthError { error: data },
                                ),
                                Kind::Validation => {
                                    operations::DeploymentsImportError::ValidationError(
                                        errors::ValidationError { error: data },
                                    )
                                }
                                Kind::Unexpected => {
                                    operations::DeploymentsImportError::UnexpectedError(
                                        errors::UnexpectedError { error: data },
                                    )
                                }
                            })
                            .into_provider_failure(),
                        )
                        .await
                        .map(|_| ()),
                }
            }
        });
}

/// Which declared error a failure is.
enum Kind {
    Auth,
    Validation,
    Unexpected,
}

/// The declared error an Operation fails with, in the words an RPC uses.
fn failure<E>(error: &AuthError, wrap: impl FnOnce(Kind, SerializableErrorData) -> E) -> E {
    let (kind, code, message) = match error {
        AuthError::NotFound(detail) | AuthError::Hidden(detail, _) => {
            (Kind::Auth, "not_found", detail.clone())
        }
        AuthError::Conflict(detail) => (Kind::Auth, "revision_conflict", detail.clone()),
        AuthError::Denied => (Kind::Auth, "access_denied", "Not authorized".to_owned()),
        AuthError::DeniedNeeding(detail) => (
            Kind::Auth,
            "access_denied",
            format!("Not authorized: {detail}"),
        ),
        AuthError::Invalid(detail) => (Kind::Validation, "invalid_request", detail.clone()),
        AuthError::ResourceLimit => (
            Kind::Auth,
            "resource_limit",
            "Resource limit reached".to_owned(),
        ),
        error => {
            tracing::warn!(%error, "service bundle operation failed");
            (
                Kind::Unexpected,
                "server_error",
                format!("Request failed: {error}"),
            )
        }
    };
    let error_type = match kind {
        Kind::Auth => errors::AuthError::TYPE,
        Kind::Validation => errors::ValidationError::TYPE,
        Kind::Unexpected => errors::UnexpectedError::TYPE,
    };
    wrap(
        kind,
        SerializableErrorData {
            id: id(),
            error_type: error_type.to_owned(),
            message,
            context: None,
            trace_id: None,
            extra: [("code".to_owned(), Value::String(code.to_owned()))]
                .into_iter()
                .collect(),
        },
    )
}

/// The caller of an Operation, who must hold both `users.manage` and
/// `config.manageSecrets` in the session that started it.
fn operator(context: &RequestContext) -> Result<String, AuthError> {
    if context.resuming {
        // Neither the bytes being exported nor those being imported survive
        // a restart of Auth.
        return Err(AuthError::Invalid(
            "Trellis restarted while this ran; run it again".into(),
        ));
    }
    let caller = context.caller.as_ref().ok_or(AuthError::Denied)?;
    for administration in [
        Administration::ManagePrincipals,
        Administration::ManageConfigSecrets,
    ] {
        if !caller.holds(administration.capability_id()) {
            return Err(AuthError::DeniedNeeding(format!(
                "moving a service needs {}",
                administration.capability_id()
            )));
        }
    }
    Ok(caller.principal_id.clone())
}

/// Hold both capabilities in current policy, as well as in the session.
fn require_operator(connection: &Connection, actor: &str) -> Result<(), AuthError> {
    for administration in [
        Administration::ManagePrincipals,
        Administration::ManageConfigSecrets,
    ] {
        SqliteAuthorizationStore::require_administration(connection, actor, administration)?;
    }
    Ok(())
}

fn rfc3339(seconds: i64) -> String {
    crate::platform::webhooks::rfc3339(seconds.saturating_mul(1_000))
}

fn io(error: std::io::Error) -> AuthError {
    AuthError::Broker(format!("service bundle spool: {error}"))
}

fn broker(error: impl std::fmt::Display) -> AuthError {
    AuthError::Broker(error.to_string())
}

fn damaged(detail: impl std::fmt::Display) -> AuthError {
    AuthError::Invalid(format!("the bundle is damaged: {detail}"))
}

// ---------------------------------------------------------------------------
// Spools: temporary files encrypted with a key that exists only in memory.

/// A temporary file that is deleted when dropped.
struct Spool {
    path: PathBuf,
    key: [u8; 32],
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn spool_key(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32-byte key"))
}

fn spool_nonce(counter: u64) -> Nonce {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(nonce)
}

impl Spool {
    /// A new, empty spool in `directory`, and the writer that fills it.
    fn create(directory: &Path) -> Result<(Arc<Self>, SpoolWriter), AuthError> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(directory).map_err(io)?;
        let mut key = [0; 32];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| AuthError::Unavailable)?;
        let path = directory.join(id());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(io)?;
        let spool = Arc::new(Self { path, key });
        let writer = SpoolWriter {
            file: std::io::BufWriter::new(file),
            key: spool_key(&key),
            counter: 0,
            pending: Vec::with_capacity(2 * CHUNK),
            hash: Sha256::new(),
            size: 0,
        };
        Ok((spool, writer))
    }

    fn reader(&self) -> Result<SpoolReader, AuthError> {
        Ok(SpoolReader {
            file: std::io::BufReader::new(std::fs::File::open(&self.path).map_err(io)?),
            key: spool_key(&self.key),
            counter: 0,
            buffer: Vec::new(),
            position: 0,
            finished: false,
        })
    }
}

/// Encrypts what is written to a spool, chunk by chunk, and hashes it.
struct SpoolWriter {
    file: std::io::BufWriter<std::fs::File>,
    key: LessSafeKey,
    counter: u64,
    pending: Vec<u8>,
    hash: Sha256,
    size: u64,
}

impl SpoolWriter {
    fn seal(&mut self, length: usize, last: bool) -> std::io::Result<()> {
        let mut chunk: Vec<u8> = self.pending.drain(..length).collect();
        self.key
            .seal_in_place_append_tag(
                spool_nonce(self.counter),
                Aad::from([u8::from(last)]),
                &mut chunk,
            )
            .map_err(|_| std::io::Error::other("spool sealing failed"))?;
        self.counter += 1;
        self.file.write_all(&[u8::from(last)])?;
        self.file
            .write_all(&u32::try_from(chunk.len()).unwrap_or(u32::MAX).to_be_bytes())?;
        self.file.write_all(&chunk)
    }

    /// Seal the last chunk. Returns the plaintext size and its SHA-256, in
    /// unpadded base64url.
    fn finish(mut self) -> Result<(u64, String), AuthError> {
        let length = self.pending.len();
        self.seal(length, true).map_err(io)?;
        self.file.flush().map_err(io)?;
        self.file.get_ref().sync_all().map_err(io)?;
        Ok((self.size, URL_SAFE_NO_PAD.encode(self.hash.finalize())))
    }
}

impl Write for SpoolWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.hash.update(data);
        self.size += data.len() as u64;
        self.pending.extend_from_slice(data);
        // The last chunk is sealed by `finish`, so a full one waits until
        // more follows it.
        while self.pending.len() > CHUNK {
            self.seal(CHUNK, false)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Decrypts a spool as it is read.
struct SpoolReader {
    file: std::io::BufReader<std::fs::File>,
    key: LessSafeKey,
    counter: u64,
    buffer: Vec<u8>,
    position: usize,
    finished: bool,
}

impl Read for SpoolReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.position == self.buffer.len() {
            if self.finished {
                return Ok(0);
            }
            let mut header = [0; 5];
            self.file.read_exact(&mut header)?;
            let last = header[0] == 1;
            let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
            if length > CHUNK + 16 {
                return Err(std::io::Error::other("spool chunk too long"));
            }
            let mut chunk = vec![0; length];
            self.file.read_exact(&mut chunk)?;
            let plain = self
                .key
                .open_in_place(
                    spool_nonce(self.counter),
                    Aad::from([u8::from(last)]),
                    &mut chunk,
                )
                .map_err(|_| std::io::Error::other("spool chunk does not authenticate"))?
                .len();
            chunk.truncate(plain);
            self.counter += 1;
            self.buffer = chunk;
            self.position = 0;
            self.finished = last;
        }
        let count = out.len().min(self.buffer.len() - self.position);
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

/// An async reader of a spool's plaintext, decrypted on the blocking pool.
fn spool_reader(spool: Arc<Spool>) -> tokio::io::DuplexStream {
    let (reader, mut writer) = tokio::io::duplex(CHUNK);
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        use tokio::io::AsyncWriteExt as _;
        let mut source = match spool.reader() {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(%error, "cannot open an export's spool");
                return;
            }
        };
        let mut buffer = vec![0; CHUNK];
        loop {
            match source.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if handle.block_on(writer.write_all(&buffer[..read])).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    // The transfer then ends short and the download fails.
                    tracing::warn!(%error, "cannot read an export's spool");
                    return;
                }
            }
        }
        let _ = handle.block_on(writer.shutdown());
    });
    reader
}

/// Exports waiting for their download.
struct Exports {
    directory: PathBuf,
    finished: Mutex<BTreeMap<String, FinishedExport>>,
}

struct FinishedExport {
    actor: String,
    spool: Arc<Spool>,
    size: u64,
    digest: String,
    expires_at: i64,
}

impl Exports {
    fn keep(&self, export_id: String, export: FinishedExport, now: i64) {
        let mut finished = self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        finished.retain(|_, export| export.expires_at > now);
        finished.insert(export_id, export);
    }

    /// The spool, size and digest of `actor`'s unexpired export.
    fn finished(
        &self,
        export_id: &str,
        actor: &str,
        now: i64,
    ) -> Option<(Arc<Spool>, u64, String)> {
        let mut finished = self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        finished.retain(|_, export| export.expires_at > now);
        finished
            .get(export_id)
            .filter(|export| export.actor == actor)
            .map(|export| {
                (
                    Arc::clone(&export.spool),
                    export.size,
                    export.digest.clone(),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// The bundle's content: named entries of length-prefixed chunks.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format: u32,
    trellis_version: String,
    exported_at: i64,
    source_installation_id: String,
    source_origin: String,
    deployment_id: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    name: String,
    size: u64,
    /// Lowercase hex.
    sha256: String,
}

/// Writes the content's entries into a spool.
struct ContentWriter {
    spool: SpoolWriter,
    entries: Vec<ManifestEntry>,
}

impl ContentWriter {
    fn new(mut spool: SpoolWriter) -> Result<Self, AuthError> {
        spool.write_all(MAGIC).map_err(io)?;
        spool.write_all(&FORMAT.to_be_bytes()).map_err(io)?;
        Ok(Self {
            spool,
            entries: Vec::new(),
        })
    }

    fn entry(&mut self) -> EntryWriter<'_> {
        EntryWriter {
            spool: &mut self.spool,
            hash: Sha256::new(),
            size: 0,
        }
    }

    fn begin(&mut self, name: &str) -> Result<EntryWriter<'_>, AuthError> {
        let length = u16::try_from(name.len()).map_err(|_| AuthError::Unavailable)?;
        self.spool.write_all(&length.to_be_bytes()).map_err(io)?;
        self.spool.write_all(name.as_bytes()).map_err(io)?;
        Ok(self.entry())
    }

    fn end(&mut self, name: &str, (size, sha256): (u64, String)) -> Result<(), AuthError> {
        self.spool.write_all(&0u32.to_be_bytes()).map_err(io)?;
        self.entries.push(ManifestEntry {
            name: name.to_owned(),
            size,
            sha256,
        });
        Ok(())
    }

    fn document(&mut self, name: &str, value: &impl Serialize) -> Result<(), AuthError> {
        let bytes = serde_json::to_vec(value)?;
        let mut entry = self.begin(name)?;
        entry.write_all(&bytes).map_err(io)?;
        let written = entry.done();
        self.end(name, written)
    }
}

/// Frames one entry's bytes as they are written, and hashes them.
struct EntryWriter<'a> {
    spool: &'a mut SpoolWriter,
    hash: Sha256,
    size: u64,
}

impl EntryWriter<'_> {
    fn done(self) -> (u64, String) {
        (self.size, hex(&self.hash.finalize()))
    }
}

impl Write for EntryWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        for chunk in data.chunks(CHUNK) {
            self.spool
                .write_all(&u32::try_from(chunk.len()).unwrap_or(0).to_be_bytes())?;
            self.spool.write_all(chunk)?;
            self.hash.update(chunk);
            self.size += chunk.len() as u64;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// The documents a bundle holds.

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeploymentDocument {
    deployment_id: String,
    title: String,
    participant_id: String,
    expires_at: Option<i64>,
    instance_policy: Option<InstancePolicy>,
    webhooks: Vec<WebhookEndpoint>,
    webhook_settings: Vec<WebhookSetting>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstancePolicy {
    retire_offline: Option<bool>,
    retire_offline_after: Option<i64>,
    retire_provisioned: Option<bool>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookEndpoint {
    endpoint: String,
    service_name: String,
    enabled: bool,
    created_at: i64,
    urls: Vec<WebhookUrl>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookUrl {
    url_id: String,
    current: bool,
    created_at: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookSetting {
    endpoint: String,
    setting: String,
    value: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstancesDocument {
    instances: Vec<BundledInstance>,
    enrollment_secrets: Vec<BundledEnrollmentSecret>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundledInstance {
    instance_id: String,
    principal_id: String,
    title: String,
    /// `active` or `disabled`.
    state: String,
    identity_public_key: String,
    enrollment_secret_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundledEnrollmentSecret {
    secret_id: String,
    title: String,
    /// The SHA-256 of the secret's text; the text was never stored.
    secret_hash: String,
    expires_at: Option<i64>,
    max_instances: i64,
    enrollments_per_hour: i64,
    current_build_only: bool,
    created_at: i64,
    created_by: String,
    updated_at: i64,
    revision: i64,
    last_used_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsDocument {
    deployment: Option<BundledSettings>,
    instances: BTreeMap<String, BundledSettings>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundledResource {
    /// `kv`, `store` or `job`.
    kind: String,
    name: String,
    resource_id: String,
    allocation_id: String,
    /// The stream a KV bucket or store is, and its snapshot entry.
    stream: Option<String>,
    entry: Option<String>,
    config: Value,
    state: Value,
    messages: u64,
    bytes: u64,
}

/// One job lifecycle message, published again as it was.
#[derive(Deserialize, Serialize)]
struct JobMessage {
    subject: String,
    headers: Vec<(String, String)>,
    payload: String,
}

const DEPLOYMENT: &str = "deployment.json";
const EVIDENCE: &str = "evidence.json";
const INSTANCES: &str = "instances.json";
const SETTINGS: &str = "settings.json";
const RESOURCES: &str = "resources.json";
const REPORT: &str = "report.json";
const JOBS: &str = "jobs.jsonl";
const MANIFEST: &str = "manifest.json";

// ---------------------------------------------------------------------------
// Export.

/// Everything an export reads from the Auth database, in one transaction.
struct ExportPlan {
    deployment: DeploymentDocument,
    evidence: Value,
    instances: InstancesDocument,
    settings: SettingsDocument,
    /// `(kind, name, resource ID, allocation ID, physical ID)` of each ready resource.
    resources: Vec<(String, String, String, String, String)>,
    roles: Vec<Value>,
    grants: Vec<Value>,
    operations: bool,
}

fn read_export(
    connection: &Connection,
    storage: &OAuthStorage,
    actor: &str,
    deployment_id: &str,
) -> Result<ExportPlan, AuthError> {
    require_operator(connection, actor)?;
    let (title, participant, kind, state, expires_at): (
        String,
        String,
        String,
        String,
        Option<i64>,
    ) = connection
        .query_row(
            "SELECT title,participant_id,participant_kind,state,expires_at \
            FROM auth_deployments WHERE deployment_id=?1",
            [deployment_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| AuthError::NotFound(format!("no deployment {deployment_id}")))?;
    if state == "revoked" {
        return Err(AuthError::NotFound(format!(
            "deployment {deployment_id} was removed"
        )));
    }
    if kind != "service" {
        return Err(AuthError::Invalid(
            "only services are exported; a device moves by activating it again".into(),
        ));
    }
    if participant.starts_with("trellis.") {
        return Err(AuthError::Invalid(
            "Trellis's own services belong to their installation and are not exported".into(),
        ));
    }
    let (revision, provided, required, optional): (u64, String, String, String) = connection
        .query_row(
            "SELECT installed_revision,provided_apis_json,required_capabilities_json,\
            optional_capabilities_json FROM auth_deployment_bindings WHERE deployment_id=?1",
            [deployment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let required: Vec<String> = decode(&required)?;
    let optional: Vec<String> = decode(&optional)?;
    // Whoever holds the bundle can create a deployment with what this one holds.
    SqliteAuthorizationStore::require_platform_for(
        connection,
        actor,
        required.iter().chain(&optional).map(String::as_str),
    )?;
    let evidence: String = connection.query_row(
        "SELECT projection_json FROM auth_installed_participants \
        WHERE participant_id=?1 AND revision=?2",
        params![participant, revision],
        |row| row.get(0),
    )?;
    let instance_policy = connection
        .query_row(
            "SELECT retire_offline,retire_offline_after,retire_provisioned \
            FROM auth_deployment_instance_policies WHERE deployment_id=?1",
            [deployment_id],
            |row| {
                Ok(InstancePolicy {
                    retire_offline: row.get(0)?,
                    retire_offline_after: row.get(1)?,
                    retire_provisioned: row.get(2)?,
                })
            },
        )
        .optional()?;
    let mut statement = connection.prepare(
        "SELECT endpoint,service_name,enabled,created_at FROM auth_webhook_endpoints \
        WHERE deployment_id=?1 ORDER BY endpoint",
    )?;
    let mut webhooks = statement
        .query_map([deployment_id], |row| {
            Ok(WebhookEndpoint {
                endpoint: row.get(0)?,
                service_name: row.get(1)?,
                enabled: row.get(2)?,
                created_at: row.get(3)?,
                urls: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for webhook in &mut webhooks {
        let mut statement = connection.prepare(
            "SELECT url_id,current,created_at FROM auth_webhook_urls \
            WHERE deployment_id=?1 AND endpoint=?2 ORDER BY created_at,url_id",
        )?;
        webhook.urls = statement
            .query_map(params![deployment_id, webhook.endpoint], |row| {
                Ok(WebhookUrl {
                    url_id: row.get(0)?,
                    current: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }
    let mut statement = connection.prepare(
        "SELECT endpoint,setting,value FROM auth_webhook_settings WHERE deployment_id=?1 \
        ORDER BY endpoint,setting",
    )?;
    let webhook_settings = statement
        .query_map([deployment_id], |row| {
            Ok(WebhookSetting {
                endpoint: row.get(0)?,
                setting: row.get(1)?,
                value: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Retired and removed instances stay behind; so does any instance whose
    // key was never registered, which no process can connect as.
    let mut statement = connection.prepare(
        "SELECT i.instance_id,i.principal_id,i.title,i.state,p.identity_public_key,\
        i.enrollment_secret_id FROM auth_instances i \
        JOIN auth_provisioned_identities p ON p.instance_id=i.instance_id AND p.state='active' \
        WHERE i.deployment_id=?1 AND i.state IN ('active','disabled') ORDER BY i.instance_id",
    )?;
    let instances = statement
        .query_map([deployment_id], |row| {
            Ok(BundledInstance {
                instance_id: row.get(0)?,
                principal_id: row.get(1)?,
                title: row.get(2)?,
                state: row.get(3)?,
                identity_public_key: row.get(4)?,
                enrollment_secret_id: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = connection.prepare(
        "SELECT secret_id,title,secret_hash,expires_at,max_instances,enrollments_per_hour,\
        current_build_only,created_at,created_by,updated_at,revision,last_used_at,revoked_at \
        FROM auth_enrollment_secrets WHERE deployment_id=?1 ORDER BY secret_id",
    )?;
    let enrollment_secrets = statement
        .query_map([deployment_id], |row| {
            Ok(BundledEnrollmentSecret {
                secret_id: row.get(0)?,
                title: row.get(1)?,
                secret_hash: row.get(2)?,
                expires_at: row.get(3)?,
                max_instances: row.get(4)?,
                enrollments_per_hour: row.get(5)?,
                current_build_only: row.get(6)?,
                created_at: row.get(7)?,
                created_by: row.get(8)?,
                updated_at: row.get(9)?,
                revision: row.get(10)?,
                last_used_at: row.get(11)?,
                revoked_at: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut settings = SettingsDocument {
        deployment: super::config::export_settings(connection, storage, deployment_id, None)?,
        instances: BTreeMap::new(),
    };
    for instance in &instances {
        if let Some(overrides) = super::config::export_settings(
            connection,
            storage,
            deployment_id,
            Some(&instance.instance_id),
        )? {
            settings
                .instances
                .insert(instance.instance_id.clone(), overrides);
        }
    }
    let mut statement = connection.prepare(
        "SELECT kind,local_name,resource_id,allocation_id,physical_id FROM auth_resources \
        WHERE owner_kind='deployment' AND owner_id=?1 AND participant_id=?2 AND state='ready' \
        ORDER BY kind,local_name",
    )?;
    let resources = statement
        .query_map(params![deployment_id, participant], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let provided: Vec<super::authorization_sessions::ProviderImplementation> = decode(&provided)?;
    let apis: Vec<String> = provided.into_iter().map(|api| api.api_id).collect();
    let apis_json = json(&apis)?;
    let mut statement = connection.prepare(
        "SELECT r.role_id,r.title,ci.capability_id,\
        (SELECT count(*) FROM auth_role_assignments a \
        WHERE a.role_id=r.role_id AND a.role_generation=r.identity_generation) \
        FROM auth_roles r \
        JOIN auth_role_capabilities rc ON rc.role_id=r.role_id \
        AND rc.role_generation=r.identity_generation \
        JOIN auth_capability_identities ci ON ci.capability_id=rc.capability_id \
        AND ci.identity_generation=rc.capability_generation \
        WHERE r.state='active' AND ci.api_id IN (SELECT value FROM json_each(?1)) \
        ORDER BY r.title,r.role_id,ci.capability_id",
    )?;
    let mut roles: BTreeMap<String, (String, Vec<String>, u64)> = BTreeMap::new();
    for row in statement.query_map([&apis_json], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })? {
        let (role, title, capability, assignments) = row?;
        roles
            .entry(role)
            .or_insert_with(|| (title, Vec::new(), assignments))
            .1
            .push(capability);
    }
    let roles = roles
        .into_values()
        .map(|(name, capabilities, assignments)| {
            json!({
                "name": name,
                "capabilities": capabilities,
                "assignments": assignments.to_string(),
            })
        })
        .collect();
    let mut statement = connection.prepare(
        "SELECT ci.capability_id,count(DISTINCT d.principal_id) FROM auth_direct_capabilities d \
        JOIN auth_capability_identities ci ON ci.capability_id=d.capability_id \
        AND ci.identity_generation=d.capability_generation \
        JOIN auth_principals p ON p.principal_id=d.principal_id \
        WHERE p.kind='user' AND p.state!='revoked' \
        AND ci.api_id IN (SELECT value FROM json_each(?1)) \
        GROUP BY ci.capability_id ORDER BY ci.capability_id",
    )?;
    let grants = statement
        .query_map([&apis_json], |row| {
            Ok(json!({
                "capability": row.get::<_, String>(0)?,
                "principals": row.get::<_, u64>(1)?.to_string(),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let operations: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_resource_binding_evidence WHERE owner_kind='deployment' \
        AND owner_id=?1 AND resource_kind='framework_operation')",
        [deployment_id],
        |row| row.get(0),
    )?;
    Ok(ExportPlan {
        deployment: DeploymentDocument {
            deployment_id: deployment_id.to_owned(),
            title,
            participant_id: participant,
            expires_at,
            instance_policy,
            webhooks,
            webhook_settings,
        },
        evidence: decode(&evidence)?,
        instances: InstancesDocument {
            instances,
            enrollment_secrets,
        },
        settings,
        resources,
        roles,
        grants,
        operations,
    })
}

/// The URLs of a bundle's webhook endpoints at `origin`.
fn webhook_urls(document: &DeploymentDocument, origin: &str) -> Vec<Value> {
    document
        .webhooks
        .iter()
        .flat_map(|webhook| {
            webhook.urls.iter().map(move |url| {
                let path = format!("/webhooks/{}/{}", webhook.service_name, url.url_id);
                json!({
                    "endpoint": webhook.endpoint,
                    "path": path,
                    "url": format!("{}{path}", origin.trim_end_matches('/')),
                })
            })
        })
        .collect()
}

/// How many messages a stream holds, and whether it exists.
async fn stream_messages(jetstream: &async_nats::jetstream::Context, name: &str) -> Option<u64> {
    let mut stream = jetstream.get_stream(name).await.ok()?;
    stream.info().await.ok().map(|info| info.state.messages)
}

async fn export(
    engine: &Arc<OAuthEngine>,
    nats: &async_nats::Client,
    exports: &Exports,
    context: &RequestContext,
    input: &operations::DeploymentsExportInput,
    operation: &OperationControl<
        trellis_rs::generated::OperationAdapter<operations::DeploymentsExport>,
    >,
) -> Result<operations::DeploymentsExportOutput, AuthError> {
    let actor = operator(context)?;
    let deployment_id = input.deployment_id.to_string();
    let storage = engine.server.store().clone();
    let store = Arc::clone(&storage.authority);
    let exported_at = crate::clock::unix_now();
    let plan = {
        let (storage, actor, deployment_id) =
            (storage.clone(), actor.clone(), deployment_id.clone());
        blocking(move || {
            storage
                .authority
                .transaction(|connection| read_export(connection, &storage, &actor, &deployment_id))
        })
        .await?
    };
    let progress = |step: &str, bytes: u64| {
        let step = step.to_owned();
        async move {
            let progress: operations::DeploymentsExportProgress =
                serde_json::from_value(json!({ "step": step, "bytes": bytes.to_string() }))?;
            operation
                .progress(progress)
                .await
                .map(|_| ())
                .map_err(|error| AuthError::Broker(error.to_string()))
        }
    };
    progress("reading the service's data", 0).await?;
    let (spool, writer) = Spool::create(&exports.directory)?;
    let mut content = ContentWriter::new(writer)?;
    content.document(DEPLOYMENT, &plan.deployment)?;
    content.document(EVIDENCE, &plan.evidence)?;
    content.document(INSTANCES, &plan.instances)?;
    content.document(SETTINGS, &plan.settings)?;
    let jetstream = async_nats::jetstream::new(nats.clone());
    let mut resources = Vec::new();
    let mut left_behind = vec![
        "job history and finished jobs".to_owned(),
        "earlier settings revisions".to_owned(),
        "webhook request records".to_owned(),
        "users, roles, role assignments, direct grants, and consent".to_owned(),
        "audit records".to_owned(),
    ];
    // Each job queue's publish prefix, by its place in `resources`.
    let mut queues = Vec::new();
    for (kind, name, resource_id, allocation_id, physical) in &plan.resources {
        match kind.as_str() {
            "kv" | "store" => {
                let stream = format!("{}_{physical}", if kind == "kv" { "KV" } else { "OBJ" });
                let entry_name = format!("resources/{resource_id}.snapshot");
                progress(&format!("copying {kind} {name}"), content.spool.size).await?;
                let mut entry = content.begin(&entry_name)?;
                let (config, state) = snapshot_stream_into(nats, &stream, false, &mut entry)
                    .await
                    .map_err(broker)?;
                let written = entry.done();
                content.end(&entry_name, written)?;
                resources.push(BundledResource {
                    kind: kind.clone(),
                    name: name.clone(),
                    resource_id: resource_id.clone(),
                    allocation_id: allocation_id.clone(),
                    stream: Some(stream),
                    entry: Some(entry_name),
                    messages: state["messages"].as_u64().unwrap_or(0),
                    bytes: state["bytes"].as_u64().unwrap_or(0),
                    config,
                    state,
                });
            }
            "job" => {
                let physical: Value = serde_json::from_str(physical)?;
                let prefix = physical["publish_prefix"]
                    .as_str()
                    .ok_or(AuthError::Unavailable)?
                    .to_owned();
                queues.push((resources.len(), prefix));
                resources.push(BundledResource {
                    kind: kind.clone(),
                    name: name.clone(),
                    resource_id: resource_id.clone(),
                    allocation_id: allocation_id.clone(),
                    stream: None,
                    entry: None,
                    config: Value::Null,
                    state: Value::Null,
                    messages: 0,
                    bytes: 0,
                });
            }
            "consumer" | "webhook" => {
                let physical: Value = serde_json::from_str(physical)?;
                if let (Some(stream), Some(consumer)) =
                    (physical["stream"].as_str(), physical["consumer"].as_str())
                {
                    let pending = match jetstream.get_stream(stream).await {
                        Ok(stream) => stream
                            .consumer_info(consumer)
                            .await
                            .map(|info| info.num_pending)
                            .unwrap_or(0),
                        Err(_) => 0,
                    };
                    if pending > 0 {
                        left_behind.push(format!(
                            "{pending} events {} {name} had not processed yet; it starts with \
                            new events",
                            if kind == "webhook" {
                                "webhook endpoint"
                            } else {
                                "consumer"
                            }
                        ));
                    }
                }
            }
            "state" => left_behind.push(format!("the values of state {name}")),
            _ => {}
        }
    }
    if !queues.is_empty() {
        progress("copying waiting jobs", content.spool.size).await?;
        let mut entry = content.begin(JOBS)?;
        for (index, prefix) in &queues {
            let (jobs, bytes) = copy_unfinished_jobs(nats, prefix, &mut entry).await?;
            resources[*index].messages = jobs;
            resources[*index].bytes = bytes;
        }
        let written = entry.done();
        content.end(JOBS, written)?;
    }
    if plan.operations {
        left_behind.push("the service's Operations, open and finished".to_owned());
    }
    if !plan.deployment.webhooks.is_empty() {
        let intake = crate::platform::webhooks::stream_name(&deployment_id);
        if let Some(waiting) = stream_messages(&jetstream, &intake)
            .await
            .filter(|count| *count > 0)
        {
            left_behind.push(format!(
                "{waiting} webhook requests waiting in the intake stream"
            ));
        }
    }
    content.document(RESOURCES, &resources)?;
    let settings = store.settings.clone();
    let origin = settings.oauth_origin();
    let report = json!({
        "deploymentId": deployment_id,
        "title": plan.deployment.title,
        "participantId": plan.deployment.participant_id,
        "sourceInstallationId": settings.instance_id,
        "sourceOrigin": origin,
        "contentDigest": "",
        "exportedAt": rfc3339(exported_at),
        "instances": plan.instances.instances.iter().map(|instance| json!({
            "instanceId": instance.instance_id,
            "title": instance.title,
            "state": instance.state,
            "identityPublicKey": instance.identity_public_key,
        })).collect::<Vec<_>>(),
        "enrollmentSecrets": plan.instances.enrollment_secrets.len().to_string(),
        "resources": resources.iter().map(|resource| json!({
            "kind": resource.kind,
            "name": resource.name,
            "messages": resource.messages.to_string(),
            "bytes": resource.bytes.to_string(),
        })).collect::<Vec<_>>(),
        "webhooks": webhook_urls(&plan.deployment, &origin),
        "roles": plan.roles,
        "grants": plan.grants,
        "leftBehind": left_behind,
    });
    content.document(REPORT, &report)?;
    let manifest = Manifest {
        format: FORMAT,
        trellis_version: env!("CARGO_PKG_VERSION").to_owned(),
        exported_at,
        source_installation_id: settings.instance_id.clone(),
        source_origin: origin,
        deployment_id: deployment_id.clone(),
        entries: content.entries.clone(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    // The manifest is not listed in itself, so it ends without `end`.
    let mut entry = content.begin(MANIFEST)?;
    entry.write_all(&manifest_bytes).map_err(io)?;
    entry.done();
    content.spool.write_all(&0u32.to_be_bytes()).map_err(io)?;
    let (size, content_digest) = content.spool.finish()?;
    let export_id = id();
    let mut report = report;
    report["contentDigest"] = Value::String(content_digest.clone());
    let audit = json!({
        "exportId": export_id,
        "contentDigest": content_digest,
        "size": size,
        "instances": plan.instances.instances.len(),
        "secrets": plan.settings.deployment.iter().chain(plan.settings.instances.values())
            .flat_map(|settings| settings.secrets.keys().cloned())
            .collect::<BTreeSet<_>>(),
        "resources": report["resources"],
    });
    {
        let (actor, deployment_id) = (actor.clone(), deployment_id.clone());
        store
            .transaction_blocking(move |connection| {
                connection.execute(
                    "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,\
                    target_id,data_json,created_at) VALUES(?1,?2,'deployment.exported',?3,?4,?5)",
                    params![id(), actor, deployment_id, audit.to_string(), exported_at],
                )?;
                Ok(())
            })
            .await?;
    }
    let expires_at = crate::clock::unix_now() + EXPORT_LIFETIME;
    exports.keep(
        export_id.clone(),
        FinishedExport {
            actor,
            spool,
            size,
            digest: content_digest,
            expires_at,
        },
        exported_at,
    );
    Ok(serde_json::from_value(json!({
        "exportId": export_id,
        "size": size.to_string(),
        "expiresAt": rfc3339(expires_at),
        "report": report,
    }))?)
}

/// Reads the `JOBS` messages under `prefix` once, oldest first, passing each
/// with its job ID and event to `each`, up to and including stream sequence
/// `until`. Returns the stream sequence of the last message read.
///
/// A batch holds at most `limit` messages, so how much is held at a time does
/// not depend on how much the queue retains.
async fn scan_jobs(
    stream: &async_nats::jetstream::stream::Stream,
    prefix: &str,
    headers_only: bool,
    until: u64,
    limit: usize,
    mut each: impl FnMut(&async_nats::jetstream::Message, &str, &str) -> Result<(), AuthError>,
) -> Result<u64, AuthError> {
    use async_nats::jetstream::consumer;
    let mut reader = stream
        .create_consumer(consumer::pull::Config {
            filter_subject: format!("{prefix}.>"),
            deliver_policy: consumer::DeliverPolicy::All,
            ack_policy: consumer::AckPolicy::None,
            headers_only,
            inactive_threshold: std::time::Duration::from_secs(60),
            ..Default::default()
        })
        .await
        .map_err(broker)?;
    let total = reader.info().await.map_err(broker)?.num_pending;
    let (mut read, mut last) = (0, 0);
    'reading: while read < total && last < until {
        let mut batch = reader
            .fetch()
            .max_messages(usize::try_from(total - read).map_or(limit, |left| left.min(limit)))
            .expires(std::time::Duration::from_secs(5))
            .messages()
            .await
            .map_err(broker)?;
        let mut received = 0;
        while let Some(message) = batch.next().await {
            let message = message.map_err(broker)?;
            received += 1;
            last = message.info().map_err(broker)?.stream_sequence;
            if last > until {
                break 'reading;
            }
            let rest = message
                .subject
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('.'))
                .unwrap_or_default();
            if let Some((job, event)) = rest.split_once('.') {
                each(&message, job, event)?;
            }
        }
        if received == 0 {
            return Err(AuthError::Broker(
                "the Jobs stream stopped sending a deployment's jobs".into(),
            ));
        }
        read += received;
    }
    let _ = stream.delete_consumer(&reader.cached_info().name).await;
    Ok(last)
}

/// Writes the lifecycle messages of each job under `prefix` whose last run
/// has not settled into `out`, oldest first, as JSON lines. Returns how many
/// such jobs there are and how many bytes were written.
///
/// The queue is read twice. The first pass reads headers only and keeps the
/// IDs of unfinished jobs: a job is unfinished from its `created` or
/// `retried` event until a settled one. The second copies those jobs'
/// messages as they arrive, up to where the first pass ended. What an export
/// holds in memory therefore follows the unfinished work, however much
/// finished history the queue retains.
async fn copy_unfinished_jobs(
    nats: &async_nats::Client,
    prefix: &str,
    out: &mut impl Write,
) -> Result<(u64, u64), AuthError> {
    let jetstream = async_nats::jetstream::new(nats.clone());
    let stream = jetstream
        .get_stream(crate::resources::JOBS_STREAM)
        .await
        .map_err(broker)?;
    // About 8 MiB a batch at the largest message the broker accepts. A byte
    // limit would end each batch with a status the client reports as an
    // error, so the bound is a message count.
    let batch = ((8 << 20) / nats.server_info().max_payload.max(1)).clamp(1, 256);
    let mut unfinished = BTreeSet::new();
    // Headers alone are small.
    let last = scan_jobs(&stream, prefix, true, u64::MAX, 256, |_, job, event| {
        if event == "created" || event == "retried" {
            unfinished.insert(job.to_owned());
        } else if SETTLED.contains(&event) {
            unfinished.remove(job);
        }
        Ok(())
    })
    .await?;
    if unfinished.is_empty() {
        return Ok((0, 0));
    }
    let mut bytes = 0;
    scan_jobs(&stream, prefix, false, last, batch, |message, job, _| {
        if !unfinished.contains(job) {
            return Ok(());
        }
        let headers = message
            .headers
            .iter()
            .flat_map(|headers| headers.iter())
            .flat_map(|(name, values)| {
                values
                    .iter()
                    .map(move |value| (name.to_string(), value.as_str().to_owned()))
            })
            .collect();
        let mut line = serde_json::to_vec(&JobMessage {
            subject: message.subject.to_string(),
            headers,
            payload: STANDARD.encode(&message.payload),
        })?;
        line.push(b'\n');
        out.write_all(&line).map_err(io)?;
        bytes += line.len() as u64;
        Ok(())
    })
    .await?;
    Ok((unfinished.len() as u64, bytes))
}

// ---------------------------------------------------------------------------
// Import.

/// A bundle's content, received and checked against its manifest.
struct Received {
    manifest: Manifest,
    documents: BTreeMap<String, Vec<u8>>,
    spools: BTreeMap<String, Arc<Spool>>,
    digest: String,
    size: u64,
}

/// Reads exactly `buffer.len()` bytes, hashing them; `false` at a clean end.
async fn read_part(
    stream: &mut (impl AsyncRead + Unpin),
    hash: &mut Sha256,
    buffer: &mut [u8],
    end_allowed: bool,
) -> Result<bool, AuthError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = stream
            .read(&mut buffer[filled..])
            .await
            .map_err(|error| AuthError::Invalid(format!("the bundle did not arrive: {error}")))?;
        if read == 0 {
            if filled == 0 && end_allowed {
                return Ok(false);
            }
            return Err(damaged("it ends partway through"));
        }
        filled += read;
    }
    hash.update(&buffer[..]);
    Ok(true)
}

async fn receive(
    stream: &mut (impl AsyncRead + Unpin),
    directory: &Path,
) -> Result<Received, AuthError> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut header = [0; 20];
    read_part(stream, &mut hash, &mut header, false).await?;
    if &header[..16] != MAGIC {
        return Err(damaged("it is not a Trellis service bundle"));
    }
    let format = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    if format != FORMAT {
        return Err(AuthError::Invalid(format!(
            "the bundle has format {format}, and this Trellis reads format {FORMAT}"
        )));
    }
    size += 20;
    let mut documents = BTreeMap::new();
    let mut spools = BTreeMap::new();
    let mut received: Vec<ManifestEntry> = Vec::new();
    let mut manifest = None;
    loop {
        let mut length = [0; 2];
        if !read_part(stream, &mut hash, &mut length, true).await? {
            break;
        }
        if manifest.is_some() {
            return Err(damaged("entries follow its manifest"));
        }
        let mut name = vec![0; usize::from(u16::from_be_bytes(length))];
        read_part(stream, &mut hash, &mut name, false).await?;
        let name = String::from_utf8(name).map_err(|_| damaged("an entry name is not text"))?;
        // Only the entries an export writes: each document once, the jobs,
        // and a snapshot named after a resource ID.
        let snapshot = name
            .strip_prefix("resources/")
            .and_then(|rest| rest.strip_suffix(".snapshot"))
            .is_some_and(|resource| {
                resource.len() == 43
                    && resource
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            });
        let document = [
            DEPLOYMENT, EVIDENCE, INSTANCES, SETTINGS, RESOURCES, REPORT, MANIFEST,
        ]
        .contains(&name.as_str());
        if !(document || snapshot || name == JOBS)
            || received.iter().any(|entry| entry.name == name)
        {
            return Err(damaged(format!("entry name {name:?}")));
        }
        if received.len() == MAX_ENTRIES {
            return Err(damaged(format!("it has more than {MAX_ENTRIES} entries")));
        }
        size += 2 + name.len() as u64;
        let mut kept = Vec::new();
        let mut spooled = if document {
            None
        } else {
            Some(Spool::create(directory)?)
        };
        let mut entry_hash = Sha256::new();
        let mut entry_size = 0u64;
        loop {
            let mut length = [0; 4];
            read_part(stream, &mut hash, &mut length, false).await?;
            size += 4;
            let length = u32::from_be_bytes(length) as usize;
            if length == 0 {
                break;
            }
            if length > MAX_CHUNK {
                return Err(damaged(format!("a chunk of {name} is too long")));
            }
            let mut chunk = vec![0; length];
            read_part(stream, &mut hash, &mut chunk, false).await?;
            size += length as u64;
            entry_hash.update(&chunk);
            entry_size += length as u64;
            match &mut spooled {
                Some((_, writer)) => writer.write_all(&chunk).map_err(io)?,
                None => {
                    if kept.len() + chunk.len() > MAX_DOCUMENT {
                        return Err(damaged(format!("{name} is too large")));
                    }
                    kept.extend_from_slice(&chunk);
                }
            }
        }
        let sha256 = hex(&entry_hash.finalize());
        if name == MANIFEST {
            manifest = Some(
                serde_json::from_slice::<Manifest>(&kept)
                    .map_err(|error| damaged(format!("its manifest: {error}")))?,
            );
            continue;
        }
        match spooled {
            Some((spool, writer)) => {
                writer.finish()?;
                spools.insert(name.clone(), spool);
            }
            None => {
                documents.insert(name.clone(), kept);
            }
        }
        received.push(ManifestEntry {
            name,
            size: entry_size,
            sha256,
        });
    }
    let manifest = manifest.ok_or_else(|| damaged("it has no manifest"))?;
    if manifest.format != FORMAT {
        return Err(damaged("its manifest names another format"));
    }
    let mut listed = manifest.entries.clone();
    listed.sort_by(|left, right| left.name.cmp(&right.name));
    received.sort_by(|left, right| left.name.cmp(&right.name));
    if listed != received {
        let wrong = listed
            .iter()
            .chain(&received)
            .find(|entry| !listed.contains(entry) || !received.contains(entry))
            .map_or_else(String::new, |entry| entry.name.clone());
        return Err(damaged(format!(
            "{wrong} does not match the SHA-256 its manifest gives"
        )));
    }
    Ok(Received {
        manifest,
        documents,
        spools,
        digest: URL_SAFE_NO_PAD.encode(hash.finalize()),
        size,
    })
}

/// The documents an import reads, parsed.
struct Bundle {
    received: Received,
    deployment: DeploymentDocument,
    evidence: trellis_idl::PackageEvidence,
    instances: InstancesDocument,
    settings: SettingsDocument,
    resources: Vec<BundledResource>,
    report: Value,
}

fn document<T: serde::de::DeserializeOwned>(
    received: &Received,
    name: &str,
) -> Result<T, AuthError> {
    let bytes = received
        .documents
        .get(name)
        .ok_or_else(|| damaged(format!("it has no {name}")))?;
    serde_json::from_slice(bytes).map_err(|error| damaged(format!("{name}: {error}")))
}

fn parse(received: Received) -> Result<Bundle, AuthError> {
    let deployment: DeploymentDocument = document(&received, DEPLOYMENT)?;
    if deployment.deployment_id != received.manifest.deployment_id {
        return Err(damaged("its manifest names another deployment"));
    }
    Ok(Bundle {
        evidence: document(&received, EVIDENCE)?,
        instances: document(&received, INSTANCES)?,
        settings: document(&received, SETTINGS)?,
        resources: document(&received, RESOURCES)?,
        report: document(&received, REPORT)?,
        deployment,
        received,
    })
}

/// The bucket a KV or store resource is, from its resource and allocation IDs.
fn bucket(resource: &BundledResource) -> String {
    format!(
        "TRELLIS_RESOURCE_{}_{}",
        resource.resource_id, resource.allocation_id
    )
}

/// The stream a KV or store resource is, and the subjects it holds.
fn resource_stream(resource: &BundledResource) -> (String, BTreeSet<String>) {
    let bucket = bucket(resource);
    if resource.kind == "kv" {
        (
            format!("KV_{bucket}"),
            BTreeSet::from([format!("$KV.{bucket}.>")]),
        )
    } else {
        (
            format!("OBJ_{bucket}"),
            BTreeSet::from([format!("$O.{bucket}.C.>"), format!("$O.{bucket}.M.>")]),
        )
    }
}

/// Whether `config`, a stream configuration in the broker's JSON, is exactly
/// the KV bucket or store `resource` is: its own name, subjects and ownership
/// marker, and nothing that reads from or writes to any other stream.
fn owned_stream(config: &Value, resource: &BundledResource) -> bool {
    let (stream, subjects) = resource_stream(resource);
    let marker = format!(
        "trellis.resource.v1:{}:{}",
        resource.resource_id, resource.allocation_id
    );
    let unset = |field: &str| match &config[field] {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    };
    config["name"] == stream.as_str()
        && config["description"] == marker.as_str()
        && config["storage"] == "file"
        && config["subjects"].as_array().is_some_and(|listed| {
            listed.len() == subjects.len()
                && listed
                    .iter()
                    .all(|subject| subject.as_str().is_some_and(|s| subjects.contains(s)))
        })
        && ["mirror", "sources", "republish", "subject_transform"]
            .into_iter()
            .all(unset)
        && matches!(config["metadata"], Value::Null | Value::Object(_))
}

/// The subject prefix of a job queue's lifecycle messages.
fn job_prefix(deployment_id: &str, resource: &BundledResource) -> Result<String, AuthError> {
    Ok(format!(
        "trellis.jobs.{}.{}_{}",
        digest(&("jobs", deployment_id))?,
        resource.name,
        resource.allocation_id
    ))
}

/// A line of `jobs.jsonl` and the headers and payload it publishes.
///
/// Every part replay sends is decoded here, so preflight refuses a line that
/// replay could not publish before anything changes.
fn job_message(line: &[u8]) -> Result<(JobMessage, async_nats::HeaderMap, Vec<u8>), AuthError> {
    let message: JobMessage =
        serde_json::from_slice(line).map_err(|error| damaged(format!("{JOBS}: {error}")))?;
    let mut headers = async_nats::HeaderMap::new();
    for (name, value) in &message.headers {
        let refuse = || damaged(format!("{JOBS}: a waiting job's header {name:?}"));
        headers.append(
            async_nats::header::HeaderName::from_str(name).map_err(|_| refuse())?,
            async_nats::header::HeaderValue::from_str(value).map_err(|_| refuse())?,
        );
    }
    let payload = STANDARD
        .decode(&message.payload)
        .map_err(|error| damaged(format!("{JOBS}: a waiting job's payload: {error}")))?;
    Ok((message, headers, payload))
}

/// One subject token an export writes: letters, digits, `-` and `_`.
fn token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Refuse a bundle that names anything outside the deployment it creates.
///
/// Every stream, bucket, allocation, webhook URL ID and job subject an import
/// creates or loads is derived from the deployment's ID and its build's
/// declarations as this installation compiles them, and the bundle's own copy
/// has to match. A bundle that names another deployment's stream, a platform
/// stream, another queue's subjects or any other subject is refused here,
/// before anything changes; so is one whose stream configuration would read
/// from or write to another stream, and one with a waiting job whose
/// headers or payload replay could not decode. It runs before `--check`
/// reports and before a real import writes anything. Runs on the blocking
/// pool: it compiles the build and reads the spooled jobs.
fn validate(bundle: &Bundle) -> Result<(), AuthError> {
    let deployment = &bundle.deployment;
    let deployment_id = deployment.deployment_id.as_str();
    let participant_id = deployment.participant_id.as_str();
    let ulid = |text: &str| ulid::Ulid::from_string(text).is_ok();
    if !ulid(deployment_id) {
        return Err(damaged("its deployment ID is not a deployment ID"));
    }
    if participant_id.starts_with("trellis.") {
        return Err(AuthError::Invalid(
            "Trellis's own services belong to their installation and are not imported".into(),
        ));
    }
    let graph = trellis_idl::compile_evidence(bundle.evidence.clone())
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let participant = graph
        .root_package()
        .participants()
        .values()
        .find(|participant| participant.identity().as_str() == participant_id)
        .ok_or_else(|| damaged(format!("its build has no participant {participant_id}")))?;
    if participant.kind() != trellis_idl::ParticipantKind::Service {
        return Err(damaged("its build is not a service"));
    }
    let declared = |name: &str, kind: &str| {
        participant
            .resources()
            .iter()
            .any(|(declared, definition)| {
                declared.as_str() == name
                    && matches!(
                        (definition, kind),
                        (trellis_idl::ResourceDefinition::Kv { .. }, "kv")
                            | (trellis_idl::ResourceDefinition::Store { .. }, "store")
                            | (trellis_idl::ResourceDefinition::Job { .. }, "job")
                            | (trellis_idl::ResourceDefinition::Webhook { .. }, "webhook")
                    )
            })
    };
    let mut secrets = BTreeSet::new();
    for secret in &bundle.instances.enrollment_secrets {
        if !ulid(&secret.secret_id) || !secrets.insert(secret.secret_id.as_str()) {
            return Err(damaged(format!("enrollment secret {}", secret.secret_id)));
        }
    }
    let mut instances = BTreeSet::new();
    for instance in &bundle.instances.instances {
        if !ulid(&instance.instance_id)
            || !ulid(&instance.principal_id)
            || !instances.insert(instance.instance_id.as_str())
            || !matches!(instance.state.as_str(), "active" | "disabled")
            || instance
                .enrollment_secret_id
                .as_deref()
                .is_some_and(|secret| !secrets.contains(secret))
        {
            return Err(damaged(format!("instance {}", instance.instance_id)));
        }
    }
    let mut endpoints = BTreeSet::new();
    for webhook in &deployment.webhooks {
        if !declared(&webhook.endpoint, "webhook")
            || !endpoints.insert(webhook.endpoint.as_str())
            || super::webhooks::service_name(&webhook.service_name) != webhook.service_name
        {
            return Err(damaged(format!("webhook endpoint {}", webhook.endpoint)));
        }
        for url in &webhook.urls {
            if URL_SAFE_NO_PAD
                .decode(&url.url_id)
                .map_or(true, |bytes| bytes.len() != 16)
            {
                return Err(damaged(format!(
                    "a URL of webhook endpoint {}",
                    webhook.endpoint
                )));
            }
        }
    }
    for setting in &deployment.webhook_settings {
        let known = serde_json::from_value(Value::String(setting.setting.clone()))
            .ok()
            .and_then(|known| super::webhooks::setting_name(&known).ok());
        let fits = match known {
            Some((name, true)) => {
                name == setting.setting && setting.endpoint == super::webhooks::DEPLOYMENT_WIDE
            }
            Some((name, false)) => {
                name == setting.setting && endpoints.contains(setting.endpoint.as_str())
            }
            None => false,
        };
        if !fits {
            return Err(damaged(format!("webhook setting {}", setting.setting)));
        }
    }
    let mut resources = BTreeSet::new();
    let mut snapshots = BTreeSet::new();
    let mut queues = BTreeSet::new();
    for resource in &bundle.resources {
        let expected = digest(&(
            "deployment",
            deployment_id,
            participant_id,
            resource.kind.as_str(),
            resource.name.as_str(),
        ))?;
        let refuse = || damaged(format!("resource {} {}", resource.kind, resource.name));
        if expected != resource.resource_id
            || !ulid(&resource.allocation_id)
            || !resources.insert(resource.resource_id.as_str())
        {
            return Err(refuse());
        }
        if !matches!(resource.kind.as_str(), "kv" | "store" | "job")
            || !declared(&resource.name, &resource.kind)
        {
            return Err(damaged(format!(
                "its build declares no {} {}",
                resource.kind, resource.name
            )));
        }
        if resource.kind == "job" {
            if resource.stream.is_some() || resource.entry.is_some() {
                return Err(refuse());
            }
            queues.insert(job_prefix(deployment_id, resource)?);
            continue;
        }
        let entry = format!("resources/{}.snapshot", resource.resource_id);
        if resource.stream.as_deref() != Some(resource_stream(resource).0.as_str())
            || resource.entry.as_deref() != Some(entry.as_str())
            || !owned_stream(&resource.config, resource)
            || !bundle.received.spools.contains_key(&entry)
        {
            return Err(refuse());
        }
        snapshots.insert(entry);
    }
    if let Some(name) = bundle
        .received
        .spools
        .keys()
        .find(|name| name.as_str() != JOBS && !snapshots.contains(*name))
    {
        return Err(damaged(format!("{name} belongs to no resource")));
    }
    let Some(jobs) = bundle.received.spools.get(JOBS) else {
        return Ok(());
    };
    let mut lines = std::io::BufReader::new(jobs.reader()?);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = std::io::BufRead::read_until(
            &mut std::io::Read::take(&mut lines, MAX_JOB_LINE as u64 + 1),
            b'\n',
            &mut line,
        )
        .map_err(io)?;
        if read == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            return Err(damaged(format!(
                "{JOBS} has a line over {MAX_JOB_LINE} bytes"
            )));
        }
        let (message, ..) = job_message(&line)?;
        // `<queue prefix>.<job ID>.<event>`, for one of the bundle's queues.
        let queued = message.subject.rsplitn(3, '.').collect::<Vec<_>>();
        let fits = matches!(queued.as_slice(), [event, job, prefix]
            if token(event) && token(job) && queues.contains(*prefix));
        let headers = message.headers.iter().all(|(name, _)| {
            !name.to_ascii_lowercase().starts_with("nats-")
                || name.eq_ignore_ascii_case("Nats-Msg-Id")
        });
        if !fits || !headers {
            return Err(damaged(format!(
                "a waiting job's message names {}, which is not one of the service's queues",
                message.subject
            )));
        }
    }
}

/// Why this installation cannot take the bundle, naming what is in the way.
fn conflicts(connection: &Connection, bundle: &Bundle) -> Result<(), AuthError> {
    let deployment = &bundle.deployment;
    let refuse = |detail: String| Err(AuthError::Conflict(detail));
    if let Some(title) = connection
        .query_row(
            "SELECT title FROM auth_deployments WHERE deployment_id=?1",
            [&deployment.deployment_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return refuse(format!(
            "deployment {} ({title}) already exists in this installation, and an import \
            never replaces a deployment",
            deployment.deployment_id
        ));
    }
    if let Some((existing, title)) = connection
        .query_row(
            "SELECT deployment_id,title FROM auth_deployments \
            WHERE participant_id=?1 AND state!='revoked'",
            [&deployment.participant_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        return refuse(format!(
            "{} is already deployed here as deployment {existing} ({title})",
            deployment.participant_id
        ));
    }
    for instance in &bundle.instances.instances {
        let key = super::deployments::identity_key_id(&instance.identity_public_key)?;
        let taken: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_instances WHERE instance_id=?1) \
            OR EXISTS(SELECT 1 FROM auth_principals WHERE principal_id=?2) \
            OR EXISTS(SELECT 1 FROM auth_provisioned_identities \
            WHERE identity_key_id=?3 OR identity_public_key=?4) \
            OR EXISTS(SELECT 1 FROM auth_identity_tombstones WHERE identity_key_id=?3)",
            params![
                instance.instance_id,
                instance.principal_id,
                key,
                instance.identity_public_key
            ],
            |row| row.get(0),
        )?;
        if taken {
            return refuse(format!(
                "instance {} ({}) or its identity key is already registered in this installation",
                instance.instance_id, instance.title
            ));
        }
    }
    for secret in &bundle.instances.enrollment_secrets {
        let taken: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_enrollment_secrets \
            WHERE secret_id=?1 OR secret_hash=?2)",
            params![secret.secret_id, secret.secret_hash],
            |row| row.get(0),
        )?;
        if taken {
            return refuse(format!(
                "enrollment secret {} ({}) is already registered in this installation",
                secret.secret_id, secret.title
            ));
        }
    }
    for webhook in &deployment.webhooks {
        for url in &webhook.urls {
            let taken: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_webhook_urls WHERE url_id=?1)",
                [&url.url_id],
                |row| row.get(0),
            )?;
            if taken {
                return refuse(format!(
                    "the URL of webhook endpoint {} is already in use in this installation",
                    webhook.endpoint
                ));
            }
        }
    }
    Ok(())
}

/// Commit the deployment, everything Auth keeps for it, and its resources as
/// pending, in the caller's transaction.
fn apply(
    connection: &Connection,
    storage: &OAuthStorage,
    bundle: &Bundle,
    input: &operations::DeploymentsImportInput,
    import_id: &str,
    actor: &str,
    now: i64,
) -> Result<(), AuthError> {
    let store = &storage.authority;
    require_operator(connection, actor)?;
    conflicts(connection, bundle)?;
    let deployment = &bundle.deployment;
    let deployment_id = deployment.deployment_id.as_str();
    let participant_id = deployment.participant_id.as_str();
    let identity = Mutation {
        actor: actor.to_owned(),
        request_id: id(),
        request_digest: digest(&bundle.received.digest)?,
        session: None,
    };
    super::service_updates::accept_participant_apis(
        connection,
        store,
        &identity,
        &bundle.evidence,
        participant_id,
        now,
    )?;
    let provisioning = super::rpc_deployments::deployment_input(
        connection,
        &serde_json::from_value(serde_json::to_value(&bundle.evidence)?)?,
        participant_id,
        json!({}),
    )?;
    let graph = trellis_idl::compile_evidence(bundle.evidence.clone())
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let participant = graph
        .root_package()
        .participants()
        .values()
        .find(|participant| participant.identity().as_str() == participant_id)
        .ok_or_else(|| damaged(format!("its build has no participant {participant_id}")))?;
    if participant.kind() != trellis_idl::ParticipantKind::Service {
        return Err(damaged("its build is not a service"));
    }
    super::service_updates::require_sharing_acknowledged(
        super::service_updates::shared_apis_of(
            connection,
            "",
            provisioning.expected_api_generations.keys().cloned(),
        )?,
        input.acknowledge_shared_apis,
    )?;
    super::service_updates::require_webhooks_acknowledged(
        super::service_updates::unauthenticated_webhooks(participant),
        input.acknowledge_unauthenticated_webhooks,
    )?;
    connection.execute(
        "INSERT INTO auth_deployments(deployment_id,participant_id,participant_kind,state,\
        expires_at,title) VALUES(?1,?2,'service','active',?3,?4)",
        params![
            deployment_id,
            participant_id,
            deployment.expires_at,
            deployment.title
        ],
    )?;
    SqliteAuthorizationStore::install_deployment_in_transaction(
        connection,
        actor,
        &provisioning,
        Some(deployment_id),
        &deployment.title,
        now,
    )?;
    if let Some(policy) = &deployment.instance_policy {
        connection.execute(
            "INSERT INTO auth_deployment_instance_policies(deployment_id,retire_offline,\
            retire_offline_after,retire_provisioned,revision) VALUES(?1,?2,?3,?4,1)",
            params![
                deployment_id,
                policy.retire_offline,
                policy.retire_offline_after,
                policy.retire_provisioned
            ],
        )?;
    }
    for secret in &bundle.instances.enrollment_secrets {
        connection.execute(
            "INSERT INTO auth_enrollment_secrets(secret_id,deployment_id,title,secret_hash,\
            expires_at,max_instances,enrollments_per_hour,current_build_only,created_at,\
            created_by,updated_at,revision,last_used_at,revoked_at) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                secret.secret_id,
                deployment_id,
                secret.title,
                secret.secret_hash,
                secret.expires_at,
                secret.max_instances,
                secret.enrollments_per_hour,
                secret.current_build_only,
                secret.created_at,
                secret.created_by,
                secret.updated_at,
                secret.revision,
                secret.last_used_at,
                secret.revoked_at
            ],
        )?;
    }
    for instance in &bundle.instances.instances {
        SqliteAuthorizationStore::provision_instance_in_transaction(
            connection,
            deployment_id,
            Some(&instance.identity_public_key),
            &instance.title,
            Some((&instance.principal_id, &instance.instance_id)),
            now,
        )?;
        connection.execute(
            "UPDATE auth_instances SET enrollment_secret_id=?2 WHERE instance_id=?1",
            params![instance.instance_id, instance.enrollment_secret_id],
        )?;
        if instance.state == "disabled" {
            connection.execute(
                "UPDATE auth_instances SET state='disabled' WHERE instance_id=?1",
                [&instance.instance_id],
            )?;
            connection.execute(
                "UPDATE auth_principals SET state='disabled',disabled_at=?2 WHERE principal_id=?1",
                params![instance.principal_id, now],
            )?;
        }
    }
    if let Some(settings) = &bundle.settings.deployment {
        super::config::import_settings(
            connection,
            storage,
            deployment_id,
            None,
            settings,
            actor,
            now,
        )?;
    }
    for (instance, settings) in &bundle.settings.instances {
        if !bundle
            .instances
            .instances
            .iter()
            .any(|bundled| &bundled.instance_id == instance)
        {
            return Err(damaged(format!(
                "settings name an unknown instance {instance}"
            )));
        }
        super::config::import_settings(
            connection,
            storage,
            deployment_id,
            Some(instance),
            settings,
            actor,
            now,
        )?;
    }
    for webhook in &deployment.webhooks {
        connection.execute(
            "INSERT INTO auth_webhook_endpoints(deployment_id,endpoint,service_name,enabled,\
            created_at) VALUES(?1,?2,?3,?4,?5)",
            params![
                deployment_id,
                webhook.endpoint,
                webhook.service_name,
                webhook.enabled,
                webhook.created_at
            ],
        )?;
        for url in &webhook.urls {
            connection.execute(
                "INSERT INTO auth_webhook_urls(url_id,deployment_id,endpoint,current,created_at) \
                VALUES(?1,?2,?3,?4,?5)",
                params![
                    url.url_id,
                    deployment_id,
                    webhook.endpoint,
                    url.current,
                    url.created_at
                ],
            )?;
        }
    }
    for setting in &deployment.webhook_settings {
        connection.execute(
            "INSERT INTO auth_webhook_settings(deployment_id,endpoint,setting,value) \
            VALUES(?1,?2,?3,?4)",
            params![
                deployment_id,
                setting.endpoint,
                setting.setting,
                setting.value
            ],
        )?;
    }
    // The resource and allocation IDs name the streams, so keeping both keeps
    // every KV revision and job subject as it was.
    let binding_revision: u64 = connection.query_row(
        "SELECT revision FROM auth_deployment_bindings WHERE deployment_id=?1",
        [deployment_id],
        |row| row.get(0),
    )?;
    // `validate` checked each resource against the build and the deployment.
    for resource in &bundle.resources {
        connection.execute(
            "INSERT INTO auth_resources(resource_id,allocation_id,owner_kind,owner_id,\
            participant_id,kind,local_name,commitment_json,state,binding_revision,revision,\
            created_at,updated_at,physical_id) \
            VALUES(?1,?2,'deployment',?3,?4,?5,?6,'{}','pending',?7,1,?8,?8,?9)",
            params![
                resource.resource_id,
                resource.allocation_id,
                deployment_id,
                participant_id,
                resource.kind,
                resource.name,
                binding_revision,
                now,
                bucket(resource)
            ],
        )?;
    }
    let restores: BTreeSet<&str> = bundle
        .resources
        .iter()
        .filter(|resource| resource.stream.is_some())
        .map(|resource| resource.resource_id.as_str())
        .collect();
    connection.execute(
        "INSERT INTO auth_service_imports(deployment_id,import_id,content_digest,state,\
        restores_json,created_json,loaded_json,report_json,imported_by,imported_at) \
        VALUES(?1,?7,?2,'loading',?3,'{}','[]',?4,?5,?6)",
        params![
            deployment_id,
            bundle.received.digest,
            json(&restores)?,
            bundle.report.to_string(),
            actor,
            now,
            import_id
        ],
    )?;
    connection.execute(
        "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,target_id,data_json,\
        created_at) VALUES(?1,?2,'deployment.imported',?3,?4,?5)",
        params![
            id(),
            actor,
            deployment_id,
            json!({
                "contentDigest": bundle.received.digest,
                "sourceInstallationId": bundle.received.manifest.source_installation_id,
                "sourceOrigin": bundle.received.manifest.source_origin,
                "instances": bundle.instances.instances.len(),
                "secrets": bundle.settings.deployment.iter()
                    .chain(bundle.settings.instances.values())
                    .flat_map(|settings| settings.secrets.keys().cloned())
                    .collect::<BTreeSet<_>>(),
            })
            .to_string(),
            now
        ],
    )?;
    Ok(())
}

async fn import(
    engine: &Arc<OAuthEngine>,
    nats: &async_nats::Client,
    directory: &Path,
    context: &RequestContext,
    input: &operations::DeploymentsImportInput,
    operation: &OperationControl<
        trellis_rs::generated::OperationAdapter<operations::DeploymentsImport>,
    >,
) -> Result<operations::DeploymentsImportOutput, AuthError> {
    let actor = operator(context)?;
    let progress = |step: &str, bytes: u64| {
        let step = step.to_owned();
        async move {
            let progress: operations::DeploymentsImportProgress =
                serde_json::from_value(json!({ "step": step, "bytes": bytes.to_string() }))?;
            operation
                .progress(progress)
                .await
                .map(|_| ())
                .map_err(|error| AuthError::Broker(error.to_string()))
        }
    };
    let mut stream = operation
        .take_upload_stream()
        .ok_or_else(|| AuthError::Invalid("send the bundle with the import".into()))?;
    progress("receiving the bundle", 0).await?;
    let cancellation = operation.cancellation();
    let received = tokio::select! {
        received = receive(&mut stream, directory) => received?,
        _ = cancellation.cancelled() => {
            return Err(AuthError::Invalid("the import was cancelled before the bundle arrived".into()));
        }
    };
    operation
        .upload()
        .await
        .map_err(|error| AuthError::Invalid(format!("the bundle did not arrive: {error}")))?;
    let bundle = parse(received)?;
    let storage = engine.server.store().clone();
    let store = Arc::clone(&storage.authority);
    let origin = store.settings.oauth_origin();
    let mut report = bundle.report.clone();
    report["contentDigest"] = Value::String(bundle.received.digest.clone());
    report["webhooks"] = Value::Array(webhook_urls(&bundle.deployment, &origin));
    let output = |applied: bool, resumed: bool, report: &Value| {
        serde_json::from_value(json!({
            "applied": applied,
            "resumed": resumed,
            "report": report,
        }))
        .map_err(AuthError::from)
    };
    let deployment_id = bundle.deployment.deployment_id.clone();
    let bundle = Arc::new(bundle);
    // Before anything changes, and again for an attempt that resumes.
    let checked = Arc::clone(&bundle);
    if let Err(error) = blocking(move || validate(&checked)).await {
        return Err(refused(&store, &actor, &deployment_id, error).await);
    }
    // An import of this same bundle that stopped while loading resumes.
    let earlier: Option<(String, String, String, String, String)> = {
        let deployment_id = deployment_id.clone();
        store
            .transaction_blocking(move |connection| {
                Ok(connection
                    .query_row(
                        "SELECT content_digest,state,import_id,created_json,loaded_json \
                        FROM auth_service_imports WHERE deployment_id=?1",
                        [deployment_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .await?
    };
    let resuming = match &earlier {
        Some((digest, state, ..)) if *digest == bundle.received.digest && state == "loading" => {
            true
        }
        Some((digest, state, ..)) if *digest == bundle.received.digest && state == "complete" => {
            return Err(refused(
                &store,
                &actor,
                &deployment_id,
                AuthError::Conflict(format!(
                    "this bundle was already imported as deployment {deployment_id}"
                )),
            )
            .await);
        }
        _ => false,
    };
    let jetstream = async_nats::jetstream::new(nats.clone());
    let import_id = match &earlier {
        Some((_, _, import_id, ..)) if resuming => import_id.clone(),
        _ => id(),
    };
    if !resuming {
        // What the Auth database names is reported before what NATS holds,
        // so an existing deployment is named rather than one of its streams.
        let checked = Arc::clone(&bundle);
        if let Err(error) = store
            .transaction_blocking(move |connection| conflicts(connection, &checked))
            .await
        {
            return Err(refused(&store, &actor, &deployment_id, error).await);
        }
        for resource in &bundle.resources {
            if let Some(stream) = &resource.stream {
                if jetstream.get_stream(stream).await.is_ok() {
                    return Err(refused(
                        &store,
                        &actor,
                        &deployment_id,
                        AuthError::Conflict(format!(
                            "stream {stream} of {} {} already exists in this installation's NATS",
                            resource.kind, resource.name
                        )),
                    )
                    .await);
                }
            }
        }
        let now = crate::clock::unix_now();
        let attempt = {
            let (storage, bundle, input, import_id, actor, check) = (
                storage.clone(),
                Arc::clone(&bundle),
                input.clone(),
                import_id.clone(),
                actor.clone(),
                input.check == Some(true),
            );
            blocking(move || {
                let run = |connection: &Connection| {
                    apply(
                        connection, &storage, &bundle, &input, &import_id, &actor, now,
                    )
                };
                if check {
                    storage.authority.rehearse(run)
                } else {
                    storage.authority.transaction(run)
                }
            })
            .await
        };
        if let Err(error) = attempt {
            return Err(if input.check == Some(true) {
                error
            } else {
                refused(&store, &actor, &deployment_id, error).await
            });
        }
        if input.check == Some(true) {
            return output(false, false, &report);
        }
    } else if input.check == Some(true) {
        return output(false, true, &report);
    }
    let (mut created, mut loaded): (BTreeMap<String, String>, BTreeSet<String>) = match &earlier {
        Some((_, _, _, created, loaded)) if resuming => (decode(created)?, decode(loaded)?),
        _ => (BTreeMap::new(), BTreeSet::new()),
    };
    // Each step is recorded before the next begins, so a later attempt knows
    // which streams this import made.
    let record = |column: &'static str, names: Result<String, AuthError>| {
        let deployment_id = deployment_id.clone();
        let store = Arc::clone(&store);
        async move {
            let names = names?;
            store
                .transaction_blocking(move |connection| {
                    connection.execute(
                        &format!(
                            "UPDATE auth_service_imports SET {column}=?2 WHERE deployment_id=?1"
                        ),
                        params![deployment_id, names],
                    )?;
                    Ok(())
                })
                .await
        }
    };
    for resource in &bundle.resources {
        let (Some(stream), Some(entry)) = (&resource.stream, &resource.entry) else {
            continue;
        };
        if loaded.contains(&resource.resource_id) {
            continue;
        }
        progress(
            &format!("loading {} {}", resource.kind, resource.name),
            bundle.received.size,
        )
        .await?;
        match jetstream.get_stream(stream).await {
            // A stream an earlier attempt of this import restored and did not
            // finish loading is deleted and restored again. Only one that
            // shows it is that stream: it carries this import's ID, which
            // nobody learns before the import's own stream exists, and the
            // creation time recorded once the broker made it. Any other
            // stream under the name is someone else's, and stays as it is.
            Ok(existing) => {
                let info = existing.cached_info();
                let ours = info.config.metadata.get(IMPORT_MARKER) == Some(&import_id)
                    && created.get(&resource.resource_id) == Some(&info.created.to_string());
                if !ours {
                    return Err(AuthError::Conflict(format!(
                        "stream {stream} of {} {} exists, and this import did not create it; \
                        it was left as it is",
                        resource.kind, resource.name
                    )));
                }
                jetstream.delete_stream(stream).await.map_err(broker)?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    GetStreamErrorKind::JetStream(error)
                        if error.error_code() == ErrorCode::STREAM_NOT_FOUND
                ) => {}
            Err(error) => return Err(broker(error)),
        }
        let spool = bundle
            .received
            .spools
            .get(entry)
            .ok_or_else(|| damaged(format!("it has no {entry}")))?;
        let mut snapshot = spool.reader()?;
        // The broker gives the restored stream this configuration, and with
        // it the import's ID.
        let mut config = resource.config.clone();
        config["metadata"][IMPORT_MARKER] = Value::String(import_id.clone());
        restore_stream_from(nats, stream, &config, &resource.state, &mut snapshot)
            .await
            .map_err(broker)?;
        // The broker refuses to restore over an existing stream, so the one
        // there now is this import's. Its configuration has to be the
        // resource's, with this import's ID.
        let restored = jetstream
            .get_stream(stream)
            .await
            .map_err(broker)?
            .cached_info()
            .clone();
        if restored.config.metadata.get(IMPORT_MARKER) != Some(&import_id)
            || !owned_stream(&serde_json::to_value(&restored.config)?, resource)
        {
            jetstream.delete_stream(stream).await.map_err(broker)?;
            return Err(damaged(format!(
                "the snapshot of {} {} configures another stream",
                resource.kind, resource.name
            )));
        }
        created.insert(resource.resource_id.clone(), restored.created.to_string());
        record("created_json", json(&created)).await?;
        loaded.insert(resource.resource_id.clone());
        record("loaded_json", json(&loaded)).await?;
    }
    let now = crate::clock::unix_now();
    // The resources are prepared for an instance that can run now, as this
    // installation has it: one provisioned after an earlier attempt of this
    // bundle counts, so running the bundle again finishes the import.
    let active: bool = {
        let deployment_id = deployment_id.clone();
        store
            .transaction_blocking(move |connection| {
                Ok(connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auth_deployments d \
                    JOIN auth_instances i ON i.deployment_id=d.deployment_id \
                    JOIN auth_principals p ON p.principal_id=i.principal_id \
                    WHERE d.deployment_id=?1 AND d.state='active' AND i.state='active' \
                    AND p.state='active')",
                    [deployment_id],
                    |row| row.get(0),
                )?)
            })
            .await?
    };
    let mut left = Vec::new();
    if active {
        progress("preparing the service's resources", bundle.received.size).await?;
        super::resources::materialize_native_kv(&store, nats, &deployment_id, now).await?;
        if !loaded.contains(JOBS) {
            if let Some(spool) = bundle.received.spools.get(JOBS) {
                progress("queueing waiting jobs", bundle.received.size).await?;
                let lines = std::io::BufReader::new(spool.reader()?);
                for line in std::io::BufRead::lines(lines) {
                    let (message, headers, payload) = job_message(line.map_err(io)?.as_bytes())?;
                    jetstream
                        .publish_with_headers(message.subject, headers, payload.into())
                        .await
                        .map_err(broker)?
                        .await
                        .map_err(broker)?;
                }
            }
            loaded.insert(JOBS.to_owned());
            record("loaded_json", json(&loaded)).await?;
        }
    } else {
        left.push(
            "the deployment has no active instance here yet, so its resources become ready, \
            and its waiting jobs move, once an instance is provisioned or enabled and the \
            import is run again"
                .to_owned(),
        );
    }
    if let Some(left_behind) = report["leftBehind"].as_array_mut() {
        left_behind.extend(left.into_iter().map(Value::String));
    }
    if active {
        let (deployment_id, report_json) = (deployment_id.clone(), report.to_string());
        store
            .transaction_blocking(move |connection| {
                connection.execute(
                    "UPDATE auth_service_imports SET state='complete',completed_at=?2,\
                    report_json=?3 WHERE deployment_id=?1",
                    params![deployment_id, now, report_json],
                )?;
                Ok(())
            })
            .await?;
    }
    output(true, resuming, &report)
}

/// Record a refused import in the audit and return why.
async fn refused(
    store: &Arc<SqliteAuthorizationStore>,
    actor: &str,
    deployment: &str,
    error: AuthError,
) -> AuthError {
    let (actor, deployment, reason) = (actor.to_owned(), deployment.to_owned(), error.to_string());
    let recorded = store
        .transaction_blocking(move |connection| {
            connection.execute(
                "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,target_id,\
                data_json,created_at) VALUES(?1,?2,'deployment.importRefused',?3,?4,?5)",
                params![
                    id(),
                    actor,
                    deployment,
                    json!({ "reason": reason }).to_string(),
                    crate::clock::unix_now()
                ],
            )?;
            Ok(())
        })
        .await;
    if let Err(audit) = recorded {
        tracing::warn!(error = %audit, "cannot audit a refused import");
    }
    error
}
