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
use std::io::{Read as _, Write as _};
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
fn databases(config: &RuntimeConfig) -> Result<Vec<(&'static str, PathBuf)>, BackupError> {
    let mut databases = Vec::new();
    for (name, backend) in [
        ("platform", config.platform_storage_backend()),
        ("jobs", config.jobs_storage_backend()),
        ("health", config.health_storage_backend()),
        ("events", config.events_storage_backend()),
    ] {
        let StorageBackend::Sqlite(storage) = backend?;
        databases.push((name, storage.path));
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
    let failed = || nats_error(format!("snapshot of stream {stream}"));
    let inbox = client.new_inbox();
    let mut chunks = client
        .subscribe(inbox.clone())
        .await
        .map_err(|error| failed()(error.to_string()))?;
    let request = serde_json::json!({
        "deliver_subject": inbox,
        "no_consumers": false,
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
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut output = options.open(file).map_err(file_error("write", file))?;
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
            .map_err(file_error("write", file))?;
        if let Some(reply) = chunk.reply {
            client
                .publish(reply, Bytes::new())
                .await
                .map_err(|error| failed()(error.to_string()))?;
        }
    }
    output.sync_all().map_err(file_error("write", file))?;
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
    let failed = || nats_error(format!("restoring stream {stream}"));
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
    let mut input = fs::File::open(file).map_err(file_error("read", file))?;
    let mut buffer = vec![0; CHUNK];
    loop {
        let read = input.read(&mut buffer).map_err(file_error("read", file))?;
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
fn copy_database(from: &Path, to: &Path) -> Result<(), BackupError> {
    let sqlite = |source| BackupError::Sqlite {
        path: from.to_path_buf(),
        source,
    };
    let source =
        rusqlite::Connection::open_with_flags(from, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite)?;
    source
        .busy_timeout(Duration::from_secs(5))
        .map_err(sqlite)?;
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
    for (subsystem, path) in &databases {
        if !path.is_file() {
            continue;
        }
        let relative = PathBuf::from(format!("databases/{subsystem}.sqlite"));
        copy_database(path, &destination.join(&relative))?;
        if *subsystem == "platform" {
            jetstream_identity = rusqlite::Connection::open(destination.join(&relative))
                .and_then(|copy| {
                    copy.query_row(
                        "SELECT jetstream_id FROM trellis_platform_store_marker WHERE id=1",
                        [],
                        |row| row.get(0),
                    )
                })
                .ok();
        }
        add(
            EntryKind::Database {
                subsystem: (*subsystem).to_owned(),
            },
            relative,
            None,
        )?;
    }
    if let Some((_, platform)) = databases.iter().find(|(name, _)| *name == "platform") {
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

    // The streams last, so they hold everything the databases refer to.
    let client = connect(config).await?;
    let nats_server = Some(client.server_info().version);
    let leases = format!("KV_{}", config.resolve_leases()?.bucket);
    let jetstream = async_nats::jetstream::new(client.clone());
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
/// A folder that already holds another installation is refused unless
/// `force`, and so is a file the configuration names outside the folder that
/// exists with other contents.
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
        let present = Secrets::load(&config_dir.join(SECRETS_FILE))?.instance_id;
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
        let Some((_, target)) = databases.iter().find(|(name, _)| name == subsystem) else {
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
    if let Some((_, platform)) = databases.iter().find(|(name, _)| *name == "platform") {
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

    // The streams wait in the data folder for the next start.
    let staged = paths.data.join(STAGED);
    if staged.exists() {
        fs::remove_dir_all(&staged).map_err(file_error("remove", &staged))?;
    }
    private_directory(&staged)?;
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
/// A stream loaded by an earlier start that stopped partway is skipped, so
/// starting again finishes the restore.
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
                JetStream holds none of them: start Trellis against a fresh JetStream store"
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
}
