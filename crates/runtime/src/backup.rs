//! Recovery points of an installation, and restoring one into a fresh place.
//!
//! [`backup`] writes a folder holding the files the installation is made of, a
//! copy of each SQLite database taken with SQLite's online backup, the issuer
//! keys and generated TLS beside the databases, a snapshot of each JetStream
//! stream in Trellis's NATS account, and `manifest.json`, written last, naming
//! every piece with its SHA-256. A folder without a manifest is not a backup.
//!
//! The databases are copied before the streams. Whatever a database refers to
//! in a stream was there before its copy, so the later stream snapshots hold
//! it. A projection can lag its stream, and a stream snapshot keeps the
//! position its projector's consumer had reached later, so loading the streams
//! drops the consumers that feed the Jobs, Health and Events projections. Each
//! then replays its stream; projecting an event a second time changes nothing.
//!
//! [`restore`] checks every checksum before it writes anything, puts the files
//! and databases in place, and stages the stream snapshots in the data folder.
//! The next start of the platform role loads them with
//! [`apply_staged_restore`], before anything else uses JetStream, into a broker
//! that must hold none of them yet. The lease bucket is not kept: leases only
//! say which process runs a role now.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::config::{ConfigError, ListenerTransport, RuntimeConfig, RuntimePathDefaults};
use crate::config::{Secrets, StorageBackend, SECRETS_FILE};

/// The file in a backup that names everything in it.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The manifest format this build writes and reads.
const FORMAT: u32 = 1;
/// The data-folder entry where a restore leaves stream snapshots for the next start.
const STAGED: &str = "restore";
/// The file in [`STAGED`] naming the streams an earlier, unfinished restore
/// of the folder may have loaded, which the next start deletes first.
const UNFINISHED: &str = "unfinished.json";
/// Bytes per snapshot or restore message, well below NATS's default payload limit.
const CHUNK: usize = 128 * 1024;
/// How long one JetStream API exchange may take.
const NATS_TIMEOUT: Duration = Duration::from_secs(60);
/// Durable consumers that feed a SQLite projection, by name prefix.
const PROJECTOR_CONSUMERS: [&str; 3] =
    ["jobs-projector-", "health-projector-", "events-projector-"];

/// Failure to back up or restore an installation.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// A file could not be read or written.
    #[error("{action} '{path}': {source}")]
    File {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The configuration could not be loaded.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// SQLite could not copy a database.
    #[error("SQLite backup of '{path}': {source}")]
    Sqlite {
        /// The database.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: rusqlite::Error,
    },
    /// NATS could not be reached or refused a snapshot or restore.
    #[error("NATS: {0}")]
    Nats(String),
    /// The backup or restore would lose or mix data, and was not done.
    #[error("{0}")]
    Refused(String),
    /// The backup is incomplete or does not match its manifest.
    #[error("the backup is damaged: {0}")]
    Damaged(String),
}

/// What a backup holds, written last as [`MANIFEST_FILE`].
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Manifest format.
    pub format: u32,
    /// The trellis-server version that took the backup.
    pub trellis_version: String,
    /// When the backup started, RFC 3339.
    pub created_at: String,
    /// When the last piece was written, RFC 3339.
    pub completed_at: String,
    /// The installation's ID, which a restore refuses to put over another's.
    pub instance_id: Option<String>,
    /// The identity binding the platform database to its JetStream store.
    pub jetstream_identity: Option<String>,
    /// The version of the NATS server the streams were read from.
    pub nats_server: Option<String>,
    /// Every piece of the backup.
    pub entries: Vec<Entry>,
}

impl Manifest {
    /// The streams in this backup.
    pub fn streams(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|entry| match &entry.kind {
            EntryKind::Stream { name, .. } => Some(name.as_str()),
            _ => None,
        })
    }
}

/// One file of a backup.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Where the file is in the backup folder.
    pub path: PathBuf,
    /// Lowercase hex SHA-256 of its contents.
    pub sha256: String,
    /// Its length.
    pub bytes: u64,
    /// Unix permission bits it had, restored with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// What it is, which says where a restore puts it.
    #[serde(flatten)]
    pub kind: EntryKind,
}

/// What a backup file is.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EntryKind {
    /// The installation's `config.toml`.
    Config,
    /// Another file in the installation folder, by its path there.
    Installation {
        /// Its path relative to the installation folder.
        relative: PathBuf,
    },
    /// A file the configuration names outside the installation folder.
    External {
        /// Its absolute path, where a restore puts it back.
        original: PathBuf,
    },
    /// A subsystem's SQLite database.
    Database {
        /// `platform`, `jobs`, `health` or `events`.
        subsystem: String,
    },
    /// A sign-in signing key beside the platform database.
    IssuerKey {
        /// Its file name.
        name: String,
    },
    /// A file of generated TLS in the data folder.
    GeneratedTls {
        /// Its file name.
        name: String,
    },
    /// A JetStream stream's snapshot, with consumers.
    Stream {
        /// The stream.
        name: String,
        /// Its configuration as the server reported it.
        config: serde_json::Value,
        /// Its state when the snapshot began.
        state: serde_json::Value,
    },
}

fn file_error(action: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> BackupError {
    let path = path.to_path_buf();
    move |source| BackupError::File {
        action,
        path,
        source,
    }
}

fn nats_error(context: impl std::fmt::Display) -> impl FnOnce(String) -> BackupError {
    move |error| BackupError::Nats(format!("{context}: {error}"))
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Creates `path` and its missing parents readable by their owner only.
fn private_directory(path: &Path) -> Result<(), BackupError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(file_error("create folder", path))
}

/// The SHA-256 and length of a file.
fn digest(path: &Path) -> Result<(String, u64), BackupError> {
    let mut file = fs::File::open(path).map_err(file_error("read", path))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    let mut bytes = 0;
    loop {
        let read = file.read(&mut buffer).map_err(file_error("read", path))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((hex, bytes))
}

#[cfg(unix)]
fn mode_of(path: &Path) -> Result<Option<u32>, BackupError> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = fs::metadata(path).map_err(file_error("inspect", path))?;
    Ok(Some(metadata.permissions().mode() & 0o777))
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> Result<Option<u32>, BackupError> {
    Ok(None)
}

/// Copies `from` to `to` through a staged file, so `to` is never half
/// written, with `mode` (owner-only when unknown).
fn place(from: &Path, to: &Path, mode: Option<u32>) -> Result<(), BackupError> {
    if let Some(parent) = to.parent() {
        if !parent.exists() {
            private_directory(parent)?;
        }
    }
    let mut staged = to.as_os_str().to_owned();
    staged.push(".restoring");
    let staged = PathBuf::from(staged);
    let _ = fs::remove_file(&staged);
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut output = options
            .open(&staged)
            .map_err(file_error("write", &staged))?;
        let mut input = fs::File::open(from).map_err(file_error("read", from))?;
        std::io::copy(&mut input, &mut output).map_err(file_error("write", &staged))?;
        output.sync_all().map_err(file_error("write", &staged))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode.unwrap_or(0o600)))
            .map_err(file_error("set permissions of", &staged))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    fs::rename(&staged, to).map_err(file_error("replace", to))
}

/// The installation's ID, as Auth uses it.
fn instance_id(config: &RuntimeConfig) -> Option<String> {
    config
        .auth
        .as_ref()
        .and_then(|auth| auth.instance_id.clone())
        .or_else(|| config.secrets.instance_id.clone())
}

/// Each subsystem's SQLite database, by name.
fn databases(
    config: &RuntimeConfig,
) -> Result<Vec<(&'static str, PathBuf, Duration)>, BackupError> {
    let mut databases = Vec::new();
    for (name, backend) in [
        ("platform", config.platform_storage_backend()),
        ("jobs", config.jobs_storage_backend()),
        ("health", config.health_storage_backend()),
        ("events", config.events_storage_backend()),
    ] {
        let StorageBackend::Sqlite(storage) = backend?;
        let busy_timeout = storage.busy_timeout();
        if name == "platform" {
            databases.push((
                "webhooks",
                storage
                    .path
                    .with_file_name(crate::storage::WEBHOOKS_DATABASE),
                busy_timeout,
            ));
        }
        databases.push((name, storage.path, busy_timeout));
    }
    Ok(databases)
}

/// The folder of signing keys Auth keeps beside its database.
fn issuer_keys(platform: &Path) -> PathBuf {
    platform.with_extension("issuer-keys")
}

/// The regular files directly in `folder`, by name; none when it is missing.
fn files_in(folder: &Path) -> Result<Vec<String>, BackupError> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(file_error("read folder", folder)(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(file_error("read folder", folder))?;
        let kind = entry
            .file_type()
            .map_err(file_error("read folder", folder))?;
        if kind.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Connects to the installation's NATS as Trellis's own user.
async fn connect(config: &RuntimeConfig) -> Result<async_nats::Client, BackupError> {
    let nats = config.resolve_nats_runtime()?;
    let options = crate::supervisor::nats_connect_options(&nats, &nats.trellis_creds)
        .map_err(|error| BackupError::Nats(error.to_string()))?
        .name("trellis-backup")
        .connection_timeout(Duration::from_secs(10));
    options.connect(nats.server_urls()).await.map_err(|error| {
        BackupError::Nats(format!(
            "cannot reach NATS at {}: {error}. A --dev installation's NATS runs only \
            while trellis-server does, so back one up while it runs",
            nats.servers
        ))
    })
}

/// Sends one JetStream API request and returns its JSON answer, refusing an
/// error answer.
async fn jetstream_api(
    client: &async_nats::Client,
    subject: String,
    body: Bytes,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let reply = client
        .send_request(
            subject,
            async_nats::Request::new()
                .payload(body)
                .timeout(Some(timeout)),
        )
        .await
        .map_err(|error| error.to_string())?;
    if reply.payload.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let answer: serde_json::Value =
        serde_json::from_slice(&reply.payload).map_err(|error| error.to_string())?;
    if let Some(error) = answer.get("error") {
        return Err(error["description"]
            .as_str()
            .unwrap_or("JetStream refused the request")
            .to_owned());
    }
    Ok(answer)
}

/// Streams a consistent snapshot of `stream`, with its consumers, into `file`.
/// Returns the stream's configuration and state.
async fn snapshot_stream(
    client: &async_nats::Client,
    stream: &str,
    file: &Path,
) -> Result<(serde_json::Value, serde_json::Value), BackupError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut output = options.open(file).map_err(file_error("write", file))?;
    let answer = snapshot_stream_into(client, stream, true, &mut output)
        .await
        .map_err(|error| error.at("write", file))?;
    output.sync_all().map_err(file_error("write", file))?;
    Ok(answer)
}

/// Why a stream could not be copied out of or into the broker.
#[derive(Debug)]
pub(crate) enum SnapshotError {
    /// NATS could not be reached or refused the snapshot or restore.
    Nats(BackupError),
    /// The snapshot's bytes could not be written or read.
    Io(std::io::Error),
}

impl SnapshotError {
    fn at(self, action: &'static str, path: &Path) -> BackupError {
        match self {
            Self::Nats(error) => error,
            Self::Io(source) => BackupError::File {
                action,
                path: path.to_path_buf(),
                source,
            },
        }
    }
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nats(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

/// Streams a consistent snapshot of `stream` into `output`, with its durable
/// consumers when `consumers` is set. Returns the stream's configuration and
/// state, which [`restore_stream_from`] needs to load it again.
pub(crate) async fn snapshot_stream_into(
    client: &async_nats::Client,
    stream: &str,
    consumers: bool,
    output: &mut impl std::io::Write,
) -> Result<(serde_json::Value, serde_json::Value), SnapshotError> {
    let failed = || {
        let what = format!("snapshot of stream {stream}");
        move |error: String| SnapshotError::Nats(nats_error(what)(error))
    };
    let inbox = client.new_inbox();
    let mut chunks = client
        .subscribe(inbox.clone())
        .await
        .map_err(|error| failed()(error.to_string()))?;
    let request = serde_json::json!({
        "deliver_subject": inbox,
        "no_consumers": !consumers,
        "chunk_size": CHUNK,
    });
    let answer = jetstream_api(
        client,
        format!("$JS.API.STREAM.SNAPSHOT.{stream}"),
        request.to_string().into(),
        NATS_TIMEOUT,
    )
    .await
    .map_err(failed())?;
    loop {
        let chunk = tokio::time::timeout(NATS_TIMEOUT, chunks.next())
            .await
            .map_err(|_| failed()("the server stopped sending it".to_owned()))?
            .ok_or_else(|| failed()("the subscription ended".to_owned()))?;
        // The last message is empty; its status says whether all went well.
        if chunk.payload.is_empty() {
            if let Some(status) = chunk.status.filter(|status| !status.is_success()) {
                return Err(failed()(format!(
                    "{} {}",
                    status.as_u16(),
                    chunk.description.unwrap_or_default()
                )));
            }
            break;
        }
        output
            .write_all(&chunk.payload)
            .map_err(SnapshotError::Io)?;
        if let Some(reply) = chunk.reply {
            client
                .publish(reply, Bytes::new())
                .await
                .map_err(|error| failed()(error.to_string()))?;
        }
    }
    Ok((answer["config"].clone(), answer["state"].clone()))
}

/// Loads a snapshot `file` into the broker as `stream`.
async fn restore_stream(
    client: &async_nats::Client,
    stream: &str,
    config: &serde_json::Value,
    state: &serde_json::Value,
    file: &Path,
) -> Result<(), BackupError> {
    let mut input = fs::File::open(file).map_err(file_error("read", file))?;
    restore_stream_from(client, stream, config, state, &mut input)
        .await
        .map_err(|error| error.at("read", file))
}

/// Loads a snapshot read from `input`, taken by [`snapshot_stream_into`],
/// into the broker as `stream`, which must not exist yet.
pub(crate) async fn restore_stream_from(
    client: &async_nats::Client,
    stream: &str,
    config: &serde_json::Value,
    state: &serde_json::Value,
    input: &mut impl std::io::Read,
) -> Result<(), SnapshotError> {
    let failed = || {
        let what = format!("restoring stream {stream}");
        move |error: String| SnapshotError::Nats(nats_error(what)(error))
    };
    let request = serde_json::json!({ "config": config, "state": state });
    let answer = jetstream_api(
        client,
        format!("$JS.API.STREAM.RESTORE.{stream}"),
        request.to_string().into(),
        NATS_TIMEOUT,
    )
    .await
    .map_err(failed())?;
    let deliver = answer["deliver_subject"]
        .as_str()
        .ok_or_else(|| failed()("the server named nowhere to send it".to_owned()))?
        .to_owned();
    let mut buffer = vec![0; CHUNK];
    loop {
        let read = input.read(&mut buffer).map_err(SnapshotError::Io)?;
        if read == 0 {
            break;
        }
        jetstream_api(
            client,
            deliver.clone(),
            Bytes::copy_from_slice(&buffer[..read]),
            NATS_TIMEOUT,
        )
        .await
        .map_err(failed())?;
    }
    // An empty message ends the transfer; the answer comes once it is loaded.
    jetstream_api(client, deliver, Bytes::new(), NATS_TIMEOUT * 5)
        .await
        .map_err(failed())?;
    Ok(())
}

/// Copies a SQLite database with SQLite's online backup, so a database a
/// running server writes is copied as one consistent state.
fn copy_database(from: &Path, to: &Path, busy_timeout: Duration) -> Result<(), BackupError> {
    let sqlite = |source| BackupError::Sqlite {
        path: from.to_path_buf(),
        source,
    };
    let source =
        rusqlite::Connection::open_with_flags(from, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite)?;
    source.busy_timeout(busy_timeout).map_err(sqlite)?;
    let mut target = rusqlite::Connection::open(to).map_err(sqlite)?;
    {
        let backup = rusqlite::backup::Backup::new(&source, &mut target).map_err(sqlite)?;
        // All pages in one step, under one read transaction: a step that
        // copies part of the database restarts when another connection
        // writes, and could restart for as long as the server is busy.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match backup.step(-1).map_err(sqlite)? {
                rusqlite::backup::StepResult::Done => break,
                _ if std::time::Instant::now() >= deadline => {
                    return Err(BackupError::Refused(format!(
                        "'{}' stayed locked for a minute; try the backup again",
                        from.display()
                    )));
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
    drop(target);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(to, fs::Permissions::from_mode(0o600))
            .map_err(file_error("set permissions of", to))?;
    }
    Ok(())
}

/// Saves a recovery point of the installation that `config` was loaded from
/// at `config_file`, with its data in `data_dir`, into the new or empty folder
/// `destination`. The server may be running. NATS must be reachable.
///
/// # Errors
///
/// Returns [`BackupError`] when `destination` is not empty, a file or
/// database can't be copied, or NATS can't be reached or snapshot a stream.
pub async fn backup(
    config: &RuntimeConfig,
    config_file: &Path,
    data_dir: &Path,
    destination: &Path,
) -> Result<Manifest, BackupError> {
    let created_at = now();
    if fs::read_dir(destination).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(BackupError::Refused(format!(
            "{} is not empty; back up into a new folder",
            destination.display()
        )));
    }
    private_directory(destination)?;
    let config_dir = config_file.parent().unwrap_or(Path::new("/"));
    let mut entries = Vec::new();
    let mut add = |kind: EntryKind, path: PathBuf, from: Option<&Path>| {
        let target = destination.join(&path);
        if let Some(from) = from {
            if let Some(parent) = target.parent() {
                private_directory(parent)?;
            }
            place(from, &target, Some(0o600))?;
        }
        let (sha256, bytes) = digest(&target)?;
        entries.push(Entry {
            path,
            sha256,
            bytes,
            mode: match from {
                Some(from) => mode_of(from)?,
                None => Some(0o600),
            },
            kind,
        });
        Ok::<_, BackupError>(())
    };

    // The files the installation is made of: its folder's own, and those
    // its configuration names.
    add(
        EntryKind::Config,
        "installation/config.toml".into(),
        Some(config_file),
    )?;
    let mut named = BTreeSet::new();
    for file in [SECRETS_FILE, "nats.conf"] {
        let path = config_dir.join(file);
        if path.is_file() {
            named.insert(path);
        }
    }
    if let Some(nats) = &config.nats {
        named.extend(
            [
                &nats.system_credentials,
                &nats.operator_signing_key,
                &nats.ca_certificate,
            ]
            .into_iter()
            .flatten()
            .cloned(),
        );
    }
    if let Some(ListenerTransport::Tls {
        certificate_chain,
        private_key,
    }) = config
        .http
        .as_ref()
        .and_then(|http| http.transport.as_ref())
    {
        named.insert(certificate_chain.clone());
        named.insert(private_key.clone());
    }
    if let Some(ca) = config
        .tls
        .as_ref()
        .and_then(|tls| tls.generated_ca_certificate.clone())
    {
        named.insert(ca);
    }
    for (index, path) in named.iter().enumerate() {
        if !path.is_file() {
            return Err(BackupError::Refused(format!(
                "the configuration names {}, which is not a file",
                path.display()
            )));
        }
        match path.strip_prefix(config_dir) {
            Ok(relative) => add(
                EntryKind::Installation {
                    relative: relative.to_path_buf(),
                },
                Path::new("installation").join(relative),
                Some(path),
            )?,
            Err(_) => add(
                EntryKind::External {
                    original: path.clone(),
                },
                Path::new("external").join(format!(
                    "{index}-{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )),
                Some(path),
            )?,
        }
    }

    // The databases, then what Auth keeps beside them.
    let databases = databases(config)?;
    private_directory(&destination.join("databases"))?;
    let mut jetstream_identity = None;
    for (subsystem, path, busy_timeout) in &databases {
        if !path.is_file() {
            continue;
        }
        let relative = PathBuf::from(format!("databases/{subsystem}.sqlite"));
        copy_database(path, &destination.join(&relative), *busy_timeout)?;
        if *subsystem == "platform" {
            let copy = destination.join(&relative);
            jetstream_identity = Some(
                rusqlite::Connection::open(&copy)
                    .and_then(|copy| {
                        copy.query_row(
                            "SELECT jetstream_id FROM trellis_platform_store_marker WHERE id=1",
                            [],
                            |row| row.get::<_, String>(0),
                        )
                    })
                    .map_err(|source| BackupError::Sqlite { path: copy, source })?,
            );
        }
        add(
            EntryKind::Database {
                subsystem: (*subsystem).to_owned(),
            },
            relative,
            None,
        )?;
    }
    if let Some((_, platform, _)) = databases.iter().find(|(name, ..)| *name == "platform") {
        let keys = issuer_keys(platform);
        for name in files_in(&keys)? {
            add(
                EntryKind::IssuerKey { name: name.clone() },
                Path::new("issuer-keys").join(&name),
                Some(&keys.join(&name)),
            )?;
        }
    }
    let tls = data_dir.join("tls");
    for name in files_in(&tls)? {
        add(
            EntryKind::GeneratedTls { name: name.clone() },
            Path::new("tls").join(&name),
            Some(&tls.join(&name)),
        )?;
    }

    // The streams last, so they hold everything the databases refer to. They
    // must be this installation's, and its platform database's: the pair is
    // checked as a start checks it, or the backup would put together data
    // that no start of Trellis accepts.
    let client = connect(config).await?;
    let nats_server = Some(client.server_info().version);
    let leases = format!("KV_{}", config.resolve_leases()?.bucket);
    let jetstream = async_nats::jetstream::new(client.clone());
    let installation = instance_id(config);
    let servers = config.resolve_nats_runtime()?.servers;
    let (_, bound, store) = crate::platform::read_broker_identity(&jetstream)
        .await
        .map_err(|error| {
            BackupError::Refused(format!(
                "cannot confirm that NATS at {servers} holds this installation's JetStream: \
                {error}"
            ))
        })?;
    let unconfirmed = |reason: String| {
        BackupError::Refused(format!(
            "NATS at {servers} {reason}, so the backup would pair data of different \
            installations; check [nats] servers and the Trellis NATS user"
        ))
    };
    match (&bound, &installation) {
        (None, _) => {
            return Err(unconfirmed(
                "has no record of the installation it belongs to; a broker gets one on \
                the platform's first start"
                    .into(),
            ))
        }
        (Some(bound), None) => {
            return Err(unconfirmed(format!(
                "belongs to installation {bound}, and this configuration names no \
                installation ID"
            )))
        }
        (Some(bound), Some(installation)) if bound != installation => {
            return Err(unconfirmed(format!(
                "belongs to installation {bound}, not this installation {installation}"
            )))
        }
        _ => {}
    }
    if let Some(identity) = &jetstream_identity {
        if store.as_ref() != Some(identity) {
            return Err(unconfirmed(
                "holds a JetStream bound to a different platform database than this one".into(),
            ));
        }
    }
    let mut names = Vec::new();
    let mut listing = jetstream.stream_names();
    while let Some(name) = listing.next().await {
        names.push(name.map_err(|error| nats_error("listing streams")(error.to_string()))?);
    }
    names.sort();
    private_directory(&destination.join("jetstream"))?;
    for name in names.into_iter().filter(|name| *name != leases) {
        let relative = PathBuf::from(format!("jetstream/{name}.tar.s2"));
        let (config, state) = snapshot_stream(&client, &name, &destination.join(&relative)).await?;
        add(
            EntryKind::Stream {
                name,
                config,
                state,
            },
            relative,
            None,
        )?;
    }
    let _ = client.drain().await;

    let manifest = Manifest {
        format: FORMAT,
        trellis_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at,
        completed_at: now(),
        instance_id: instance_id(config),
        jetstream_identity,
        nats_server,
        entries,
    };
    let path = destination.join(MANIFEST_FILE);
    trellis_bootstrap::write_private_file(
        &path,
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|error| file_error("write", &path)(std::io::Error::other(error.to_string())))?;
    Ok(manifest)
}

/// Reads a backup's manifest and checks every file it names against it.
///
/// # Errors
///
/// Returns [`BackupError::Damaged`] when the manifest is missing, of another
/// format, or a file is missing or differs from its checksum.
pub fn verify(source: &Path) -> Result<Manifest, BackupError> {
    let path = source.join(MANIFEST_FILE);
    let contents = fs::read(&path).map_err(|error| {
        BackupError::Damaged(format!(
            "{} cannot be read ({error}); a backup that never finished has none",
            path.display()
        ))
    })?;
    let manifest: Manifest = serde_json::from_slice(&contents)
        .map_err(|error| BackupError::Damaged(format!("{}: {error}", path.display())))?;
    if manifest.format != FORMAT {
        return Err(BackupError::Damaged(format!(
            "format {} is not the format {FORMAT} this trellis-server reads",
            manifest.format
        )));
    }
    for entry in &manifest.entries {
        if entry.path.is_absolute()
            || entry
                .path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(BackupError::Damaged(format!(
                "{} is outside the backup",
                entry.path.display()
            )));
        }
        let file = source.join(&entry.path);
        let (sha256, bytes) = digest(&file).map_err(|error| {
            BackupError::Damaged(format!("{} is missing: {error}", entry.path.display()))
        })?;
        if sha256 != entry.sha256 || bytes != entry.bytes {
            return Err(BackupError::Damaged(format!(
                "{} does not match its checksum",
                entry.path.display()
            )));
        }
    }
    Ok(manifest)
}

/// Puts the backup in `source` into the installation whose config file is
/// `config_file`, with data in the folders `defaults` gives unless the
/// restored configuration's `[paths]` says otherwise, and stages its streams
/// for the next start. The server must be stopped.
///
/// A folder that already holds another installation, by the ID its start
/// would use (`[auth] instance_id`, else the one in `secrets.toml`), or an
/// installation whose configuration can't be read, is refused unless `force`,
/// and so is a file the configuration names outside the folder that exists
/// with other contents.
///
/// # Errors
///
/// Returns [`BackupError`] when the backup is damaged, the restore is refused,
/// or a file can't be written.
pub fn restore(
    source: &Path,
    config_file: &Path,
    defaults: RuntimePathDefaults,
    force: bool,
) -> Result<Manifest, BackupError> {
    let manifest = verify(source)?;
    let config_dir = config_file.parent().unwrap_or(Path::new("/"));
    if !force {
        // The installation already there is the one a start of it would be:
        // its ID is the configured one, or else the one in `secrets.toml`.
        let unreadable = |error: ConfigError| {
            BackupError::Refused(format!(
                "{} holds an installation whose ID can't be read ({error}); restore into a \
                fresh folder, or pass --force to replace it",
                config_dir.display()
            ))
        };
        let present = if config_file.exists() {
            let (present, _) =
                RuntimeConfig::load_from_path_with_defaults(config_file, defaults.clone())
                    .map_err(unreadable)?;
            instance_id(&present)
        } else {
            Secrets::load(&config_dir.join(SECRETS_FILE))
                .map_err(unreadable)?
                .instance_id
        };
        if let Some(present) =
            present.filter(|present| Some(present) != manifest.instance_id.as_ref())
        {
            return Err(BackupError::Refused(format!(
                "{} holds installation {present}, not {} from the backup; restore into a \
                fresh folder, or pass --force to replace it",
                config_dir.display(),
                manifest.instance_id.as_deref().unwrap_or("without an ID")
            )));
        }
        for entry in &manifest.entries {
            if let EntryKind::External { original } = &entry.kind {
                if original.exists()
                    && digest(original).map(|(sha256, _)| sha256).ok().as_ref()
                        != Some(&entry.sha256)
                {
                    return Err(BackupError::Refused(format!(
                        "{} exists with other contents; pass --force to replace it",
                        original.display()
                    )));
                }
            }
        }
    }
    if !config_dir.exists() {
        private_directory(config_dir)?;
    }
    for entry in &manifest.entries {
        let target = match &entry.kind {
            EntryKind::Config => config_file.to_path_buf(),
            EntryKind::Installation { relative } => config_dir.join(relative),
            EntryKind::External { original } => original.clone(),
            _ => continue,
        };
        place(&source.join(&entry.path), &target, entry.mode)?;
    }

    // The restored configuration says where its data goes.
    let (config, paths) = RuntimeConfig::load_from_path_with_defaults(config_file, defaults)?;
    let databases = databases(&config)?;
    for entry in &manifest.entries {
        let EntryKind::Database { subsystem } = &entry.kind else {
            continue;
        };
        let Some((_, target, _)) = databases.iter().find(|(name, ..)| name == subsystem) else {
            return Err(BackupError::Damaged(format!(
                "unknown database {subsystem}"
            )));
        };
        // A journal left by the database this replaces would be applied to
        // the restored one.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut journal = target.as_os_str().to_owned();
            journal.push(suffix);
            let journal = PathBuf::from(journal);
            if journal.exists() {
                fs::remove_file(&journal).map_err(file_error("remove", &journal))?;
            }
        }
        place(&source.join(&entry.path), target, Some(0o600))?;
    }
    if let Some((_, platform, _)) = databases.iter().find(|(name, ..)| *name == "platform") {
        let keys = issuer_keys(platform);
        if keys.exists() {
            fs::remove_dir_all(&keys).map_err(file_error("remove", &keys))?;
        }
        for entry in &manifest.entries {
            if let EntryKind::IssuerKey { name } = &entry.kind {
                if !keys.exists() {
                    private_directory(&keys)?;
                }
                place(&source.join(&entry.path), &keys.join(name), Some(0o600))?;
            }
        }
    }
    for entry in &manifest.entries {
        if let EntryKind::GeneratedTls { name } = &entry.kind {
            place(
                &source.join(&entry.path),
                &paths.data.join("tls").join(name),
                entry.mode,
            )?;
        }
    }

    // The streams wait in the data folder for the next start. Streams still
    // staged there are of a restore whose start didn't finish, and may be in
    // NATS already; the next start deletes them before loading these. A
    // restaging that stopped partway left its list without a manifest, so
    // the list is read on its own and replaced in one rename before anything
    // else staged goes: no interruption loses a name.
    let staged = paths.data.join(STAGED);
    let mut unfinished = BTreeSet::new();
    if let Ok(contents) = fs::read(staged.join(MANIFEST_FILE)) {
        if let Ok(earlier) = serde_json::from_slice::<Manifest>(&contents) {
            unfinished.extend(earlier.streams().map(str::to_owned));
        }
    }
    if let Ok(contents) = fs::read(staged.join(UNFINISHED)) {
        if let Ok(names) = serde_json::from_slice::<Vec<String>>(&contents) {
            unfinished.extend(names);
        }
    }
    if !staged.exists() {
        private_directory(&staged)?;
    }
    if !unfinished.is_empty() {
        let partial = staged.join(format!("{UNFINISHED}.partial"));
        trellis_bootstrap::write_private_file(
            &partial,
            serde_json::to_vec(&unfinished).unwrap_or_default(),
        )
        .map_err(|error| file_error("write", &partial)(std::io::Error::other(error.to_string())))?;
        let path = staged.join(UNFINISHED);
        fs::rename(&partial, &path).map_err(file_error("replace", &path))?;
    }
    for entry in fs::read_dir(&staged).map_err(file_error("read", &staged))? {
        let entry = entry.map_err(file_error("read", &staged))?;
        let path = entry.path();
        if entry.file_name() == UNFINISHED {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .map_err(file_error("remove", &path))?;
    }
    for entry in &manifest.entries {
        if matches!(entry.kind, EntryKind::Stream { .. }) {
            place(
                &source.join(&entry.path),
                &staged.join(&entry.path),
                Some(0o600),
            )?;
        }
    }
    let path = staged.join(MANIFEST_FILE);
    trellis_bootstrap::write_private_file(
        &path,
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|error| file_error("write", &path)(std::io::Error::other(error.to_string())))?;
    Ok(manifest)
}

/// Loads the streams a restore left in `data_dir` into the installation's
/// NATS, and returns whether there were any. Run by the platform role's start,
/// after Trellis's NATS accounts are in place and before anything else uses
/// JetStream.
///
/// A stream loaded by an earlier start that stopped partway, and whose
/// snapshot it removed, is skipped. Any other interrupted restore is run
/// again: restoring once more stages every stream with the names of those the
/// unfinished one staged, and the start deletes those from NATS before
/// loading, provided NATS holds no other stream.
///
/// # Errors
///
/// Returns [`BackupError`] when the broker already holds one of the streams,
/// a snapshot is damaged, or NATS refuses one.
pub async fn apply_staged_restore(
    config: &RuntimeConfig,
    data_dir: &Path,
) -> Result<bool, BackupError> {
    use async_nats::jetstream::{context::GetStreamErrorKind, ErrorCode};

    let staged = data_dir.join(STAGED);
    let path = staged.join(MANIFEST_FILE);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(file_error("read", &path)(error)),
    };
    let manifest: Manifest = serde_json::from_slice(&contents)
        .map_err(|error| BackupError::Damaged(format!("{}: {error}", path.display())))?;
    let client = connect(config).await?;
    let jetstream = async_nats::jetstream::new(client.clone());
    let path = staged.join(UNFINISHED);
    match fs::read(&path) {
        Ok(contents) => {
            let unfinished: BTreeSet<String> = serde_json::from_slice(&contents)
                .map_err(|error| BackupError::Damaged(format!("{}: {error}", path.display())))?;
            let mut present = Vec::new();
            let mut listing = jetstream.stream_names();
            while let Some(name) = listing.next().await {
                present
                    .push(name.map_err(|error| nats_error("listing streams")(error.to_string()))?);
            }
            if let Some(other) = present.iter().find(|name| !unfinished.contains(*name)) {
                return Err(BackupError::Refused(format!(
                    "NATS has stream {other}, which no unfinished restore of this installation \
                    loaded. A restore loads its streams into a NATS whose JetStream holds none \
                    of them: start Trellis against a fresh JetStream store"
                )));
            }
            for name in &present {
                jetstream
                    .delete_stream(name)
                    .await
                    .map_err(|error| nats_error(format!("stream {name}"))(error.to_string()))?;
                tracing::info!(stream = %name, "deleted a stream an unfinished restore loaded");
            }
            fs::remove_file(&path).map_err(file_error("remove", &path))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(file_error("read", &path)(error)),
    }
    for entry in &manifest.entries {
        let EntryKind::Stream {
            name,
            config,
            state,
        } = &entry.kind
        else {
            continue;
        };
        let file = staged.join(&entry.path);
        let present = match jetstream.get_stream(name).await {
            Ok(_) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    GetStreamErrorKind::JetStream(error)
                        if error.error_code() == ErrorCode::STREAM_NOT_FOUND
                ) =>
            {
                false
            }
            Err(error) => return Err(nats_error(format!("stream {name}"))(error.to_string())),
        };
        if !file.exists() {
            if present {
                continue;
            }
            return Err(BackupError::Damaged(format!(
                "the staged snapshot of stream {name} is gone and NATS doesn't have the stream"
            )));
        }
        if present {
            return Err(BackupError::Refused(format!(
                "NATS already has stream {name}. A restore loads its streams into a NATS whose \
                JetStream holds none of them: start Trellis against a fresh JetStream store, \
                or, if an earlier start of this restore stopped partway, restore again"
            )));
        }
        let (sha256, _) = digest(&file)?;
        if sha256 != entry.sha256 {
            return Err(BackupError::Damaged(format!(
                "the staged snapshot of stream {name} does not match its checksum"
            )));
        }
        restore_stream(&client, name, config, state, &file).await?;
        fs::remove_file(&file).map_err(file_error("remove", &file))?;
        tracing::info!(stream = %name, "restored stream");
    }
    // Each projection replays its stream from the start, catching up on what
    // the stream got after its database was copied.
    for name in manifest.streams() {
        let stream = jetstream
            .get_stream(name)
            .await
            .map_err(|error| nats_error(format!("stream {name}"))(error.to_string()))?;
        let mut consumers = Vec::new();
        let mut listing = stream.consumer_names();
        while let Some(consumer) = listing.next().await {
            consumers.push(
                consumer
                    .map_err(|error| nats_error(format!("stream {name}"))(error.to_string()))?,
            );
        }
        for consumer in consumers.iter().filter(|consumer| {
            PROJECTOR_CONSUMERS
                .iter()
                .any(|prefix| consumer.starts_with(prefix))
        }) {
            stream.delete_consumer(consumer).await.map_err(|error| {
                nats_error(format!("consumer {consumer} of stream {name}"))(error.to_string())
            })?;
        }
    }
    let _ = client.drain().await;
    fs::remove_dir_all(&staged).map_err(file_error("remove", &staged))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backup of an installation made of just its config and secrets.
    fn backup_of(folder: &Path, instance: &str) {
        let mut entries = Vec::new();
        for (kind, name, contents) in [
            (EntryKind::Config, "config.toml", String::new()),
            (
                EntryKind::Installation {
                    relative: SECRETS_FILE.into(),
                },
                SECRETS_FILE,
                format!("instance_id = \"{instance}\"\n"),
            ),
        ] {
            let path = Path::new("installation").join(name);
            fs::create_dir_all(folder.join("installation")).unwrap();
            fs::write(folder.join(&path), contents).unwrap();
            let (sha256, bytes) = digest(&folder.join(&path)).unwrap();
            entries.push(Entry {
                path,
                sha256,
                bytes,
                mode: Some(0o600),
                kind,
            });
        }
        let manifest = Manifest {
            format: FORMAT,
            trellis_version: "test".into(),
            created_at: now(),
            completed_at: now(),
            instance_id: Some(instance.into()),
            jetstream_identity: None,
            nats_server: None,
            entries,
        };
        fs::write(
            folder.join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn defaults(root: &Path) -> RuntimePathDefaults {
        RuntimePathDefaults {
            data: root.join("var"),
            state: root.join("var"),
            cache: root.join("cache"),
            runtime: root.join("var/run"),
            logs: root.join("var/log"),
        }
    }

    #[test]
    fn a_restore_refuses_a_folder_holding_another_installation_unless_forced() {
        let backup = tempfile::tempdir().unwrap();
        backup_of(backup.path(), "01KBACKUPINSTALLATION0000000");
        let installation = tempfile::tempdir().unwrap();
        let secrets = installation.path().join(SECRETS_FILE);
        trellis_bootstrap::write_private_file(
            &secrets,
            "instance_id = \"01KOTHERINSTALLATION00000000\"\n",
        )
        .unwrap();
        let config = installation.path().join("config.toml");

        let refused = restore(backup.path(), &config, defaults(installation.path()), false);
        assert!(
            matches!(refused, Err(BackupError::Refused(_))),
            "{refused:?}"
        );
        assert!(!config.exists(), "a refused restore wrote the config");

        restore(backup.path(), &config, defaults(installation.path()), true).unwrap();
        assert_eq!(
            Secrets::load(&secrets).unwrap().instance_id.as_deref(),
            Some("01KBACKUPINSTALLATION0000000")
        );
        assert!(installation
            .path()
            .join("var")
            .join(STAGED)
            .join(MANIFEST_FILE)
            .is_file());
    }

    #[test]
    fn a_damaged_backup_is_refused_before_anything_is_written() {
        let backup = tempfile::tempdir().unwrap();
        backup_of(backup.path(), "01KBACKUPINSTALLATION0000000");
        fs::write(
            backup.path().join("installation/config.toml"),
            "# changed\n",
        )
        .unwrap();
        let installation = tempfile::tempdir().unwrap();
        let config = installation.path().join("fresh/config.toml");

        let damaged = restore(backup.path(), &config, defaults(installation.path()), false);
        assert!(
            matches!(damaged, Err(BackupError::Damaged(_))),
            "{damaged:?}"
        );
        assert!(!installation.path().join("fresh").exists());
    }

    /// An installation's ID can be in its `config.toml` alone, as
    /// `[auth] instance_id`, which a start uses before `secrets.toml`'s.
    #[test]
    fn a_restore_refuses_a_folder_whose_config_names_another_installation_unless_forced() {
        let backup = tempfile::tempdir().unwrap();
        backup_of(backup.path(), "01KBACKUPINSTALLATION0000000");
        let installation = tempfile::tempdir().unwrap();
        let config = installation.path().join("config.toml");
        let present = "[auth]\ninstance_id = \"01KOTHERINSTALLATION00000000\"\n";
        fs::write(&config, present).unwrap();

        let refused = restore(backup.path(), &config, defaults(installation.path()), false);
        assert!(
            matches!(&refused, Err(BackupError::Refused(reason))
                if reason.contains("01KOTHERINSTALLATION00000000")),
            "{refused:?}"
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), present);

        restore(backup.path(), &config, defaults(installation.path()), true).unwrap();
        let (restored, _) =
            RuntimeConfig::load_from_path_with_defaults(&config, defaults(installation.path()))
                .unwrap();
        assert_eq!(
            instance_id(&restored).as_deref(),
            Some("01KBACKUPINSTALLATION0000000")
        );
    }

    /// An installation whose ID can't be read may be another one.
    #[test]
    fn a_restore_refuses_a_folder_whose_config_cant_be_read_unless_forced() {
        let backup = tempfile::tempdir().unwrap();
        backup_of(backup.path(), "01KBACKUPINSTALLATION0000000");
        let installation = tempfile::tempdir().unwrap();
        let config = installation.path().join("config.toml");
        let present = "[auth\ninstance_id = \"01KOTHERINSTALLATION00000000\"\n";
        fs::write(&config, present).unwrap();

        let refused = restore(backup.path(), &config, defaults(installation.path()), false);
        assert!(
            matches!(refused, Err(BackupError::Refused(_))),
            "{refused:?}"
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), present);

        restore(backup.path(), &config, defaults(installation.path()), true).unwrap();
        assert_eq!(fs::read_to_string(&config).unwrap(), "");
    }

    /// Starts `config`'s installation once, so it binds its broker, and
    /// waits for it to stop.
    async fn started_once(config: &RuntimeConfig, origin: &str) {
        let (stop, mut runtime) = crate::platform::auth::tests::runtime_rotation::start_runtime(
            config,
            crate::RuntimeMode::All,
            origin,
            &[],
        )
        .await;
        stop.stop();
        tokio::time::timeout(Duration::from_secs(30), &mut runtime)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// A backup pairs the platform database with its own installation's
    /// JetStream, as a start does. Pointed at a broker no installation has
    /// bound, or at another installation's, it fails before its manifest is
    /// written, so no restore can take it for a backup.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_backup_refuses_a_broker_that_is_not_its_installations() {
        use crate::platform::auth::tests::runtime_rotation::installation;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let reserved: Vec<_> = (0..8)
            .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
            .collect();
        let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
        let our_origin = format!("http://127.0.0.1:{}", ports[0]);
        let (ours, _our_broker) = installation(
            &root.join("ours"),
            &ports[..4],
            &our_origin,
            "loopback_development",
            None,
        )
        .await;
        started_once(&ours, &our_origin).await;
        let their_origin = format!("http://127.0.0.1:{}", ports[4]);
        let (theirs, _their_broker) = installation(
            &root.join("theirs"),
            &ports[4..],
            &their_origin,
            "loopback_development",
            None,
        )
        .await;

        // Ours names their broker and its Trellis user by mistake.
        let mut misdirected = ours.clone();
        misdirected.nats.as_mut().unwrap().servers = theirs.nats.as_ref().unwrap().servers.clone();
        misdirected
            .secrets
            .nats
            .users
            .trellis
            .clone_from(&theirs.secrets.nats.users.trellis);
        let config_file = root.join("ours/config/config.toml");
        let data = root.join("ours/data");

        let unbound = root.join("unbound");
        let refused = backup(&misdirected, &config_file, &data, &unbound).await;
        assert!(
            matches!(refused, Err(BackupError::Refused(_))),
            "{refused:?}"
        );
        assert!(!unbound.join(MANIFEST_FILE).exists());

        started_once(&theirs, &their_origin).await;
        let crossed = root.join("crossed");
        let refused = backup(&misdirected, &config_file, &data, &crossed).await;
        let their_instance = instance_id(&theirs).unwrap();
        assert!(
            matches!(&refused, Err(BackupError::Refused(reason))
                if reason.contains(&their_instance)),
            "{refused:?}"
        );
        assert!(!crossed.join(MANIFEST_FILE).exists());
    }

    /// The first start after a restore can stop after NATS took a stream and
    /// before it removed that stream's snapshot, or lose NATS's answer, and
    /// the next start then refuses the stream. Restoring again, even after
    /// another restore stopped partway, and then starting finishes the
    /// restore: every stream is the backup's, and the installation serves. A
    /// stream no restore loaded still stops it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_interrupted_restore_finishes_when_restored_again() {
        use crate::platform::auth::tests::runtime_rotation::{
            installation, start_broker, start_runtime,
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let reserved: Vec<_> = (0..4)
            .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
            .collect();
        let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
        let origin = format!("http://127.0.0.1:{}", ports[0]);
        let lost = root.join("lost");
        let (config, mut broker) =
            installation(&lost, &ports, &origin, "loopback_development", None).await;
        started_once(&config, &origin).await;
        let saved = root.join("backup");
        let manifest = backup(
            &config,
            &lost.join("config/config.toml"),
            &lost.join("data"),
            &saved,
        )
        .await
        .unwrap();
        broker.stop().unwrap();

        // Restored onto a new disk, beside a new NATS with an empty store,
        // and prepared as a start prepares it before loading the streams.
        let fresh = root.join("fresh");
        let config_file = fresh.join("config/config.toml");
        let prepared = || async {
            let (mut restored, paths) =
                RuntimeConfig::load_from_path_with_defaults(&config_file, defaults(&fresh))
                    .unwrap();
            crate::installation::prepare(
                &mut restored,
                &fresh.join("config"),
                &paths.data,
                crate::RuntimeMode::All,
                true,
            )
            .await
            .unwrap();
            (restored, paths.data)
        };
        restore(&saved, &config_file, defaults(&fresh), false).unwrap();
        let _broker = start_broker(
            &fresh,
            &fresh.join("config"),
            [ports[1], ports[2], ports[3]],
        )
        .await;
        let (restored, data) = prepared().await;

        // What the start that stopped left: the streams before one holding
        // messages loaded and their snapshots removed, then that one loaded
        // with its snapshot still staged.
        let staged = data.join(STAGED);
        let streams: Vec<_> = manifest
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Stream {
                    name,
                    config,
                    state,
                } => Some((name, config, state, staged.join(&entry.path))),
                _ => None,
            })
            .collect();
        let interrupted = streams
            .iter()
            .position(|(_, _, state, _)| state["last_seq"].as_u64() > Some(0))
            .expect("a stream with messages");
        let client = connect(&restored).await.unwrap();
        for (index, (name, config, state, file)) in streams[..=interrupted].iter().enumerate() {
            restore_stream(&client, name, config, state, file)
                .await
                .unwrap();
            if index < interrupted {
                fs::remove_file(file).unwrap();
            }
        }
        let refused = apply_staged_restore(&restored, &data).await;
        assert!(
            matches!(refused, Err(BackupError::Refused(_))),
            "{refused:?}"
        );

        // A restore again that stops while copying snapshots leaves the list
        // of loaded streams and no manifest, which is written last.
        restore(&saved, &config_file, defaults(&fresh), false).unwrap();
        fs::remove_file(staged.join(MANIFEST_FILE)).unwrap();

        // Restored again, over the same installation, without --force.
        restore(&saved, &config_file, defaults(&fresh), false).unwrap();
        let (restored, data) = prepared().await;

        // A stream no restore loaded is not deleted for it.
        let jetstream = async_nats::jetstream::new(client.clone());
        jetstream
            .create_stream(async_nats::jetstream::stream::Config {
                name: "UNRELATED".into(),
                subjects: vec!["unrelated.>".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        let refused = apply_staged_restore(&restored, &data).await;
        assert!(
            matches!(&refused, Err(BackupError::Refused(reason)) if reason.contains("UNRELATED")),
            "{refused:?}"
        );
        jetstream.delete_stream("UNRELATED").await.unwrap();
        let _ = client.drain().await;

        assert!(apply_staged_restore(&restored, &data).await.unwrap());
        assert!(!staged.exists());
        let client = connect(&restored).await.unwrap();
        let jetstream = async_nats::jetstream::new(client.clone());
        for (name, _, state, _) in &streams {
            let mut stream = jetstream.get_stream(name.as_str()).await.unwrap();
            let info = stream.info().await.unwrap();
            assert_eq!(
                Some(info.state.last_sequence),
                state["last_seq"].as_u64(),
                "stream {name}"
            );
            assert_eq!(
                Some(info.state.messages),
                state["messages"].as_u64(),
                "stream {name}"
            );
        }
        let _ = client.drain().await;

        let (stop, mut runtime) =
            start_runtime(&restored, crate::RuntimeMode::All, &origin, &[]).await;
        stop.stop();
        tokio::time::timeout(Duration::from_secs(30), &mut runtime)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
