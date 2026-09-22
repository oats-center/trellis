//! Managed local NATS server support for Trellis.
//!
//! Downloads the pinned [`nats-server`](https://github.com/nats-io/nats-server) release
//! (checksum-verified), extracts it into a cache directory, and manages its lifecycle as a
//! child process. Synchronous standard-library code only; no async runtime dependency.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Pinned nats-server version and per-platform checksums, shared with the TypeScript harness
/// via this crate's `nats-binaries.json`.
const NATS_BINARIES_JSON: &str = include_str!("../nats-binaries.json");

/// How long [`ManagedNatsServer::start`] waits for all ports to accept connections.
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
/// Poll interval while waiting for readiness and graceful shutdown.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long [`ManagedNatsServer::stop`] waits for graceful shutdown before killing.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// Connect timeout for the nats-server release download.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Overall and per-body-read timeout for the nats-server release download, so DNS
/// stalls and header stalls cannot exceed the bound either.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Process-local registry serializing installation per cache directory, so concurrent
/// [`NatsServerBinary::ensure`] calls into the same cache cannot interfere. Lazily
/// initialized because `HashMap::new` is not const.
static INSTALL_LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);
/// Per-process counter making temp paths unique across calls within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
/// Test-only count of real archive downloads, used to prove install serialization.
#[cfg(test)]
static DOWNLOAD_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// Errors from managed local NATS operations.
#[derive(Debug, Error)]
pub enum LocalNatsError {
    /// The host OS/architecture has no pinned nats-server release.
    #[error(
        "unsupported platform {os}/{arch}: managed NATS supports linux and macOS on amd64 and arm64"
    )]
    UnsupportedPlatform {
        /// Operating system name from `std::env::consts::OS`.
        os: String,
        /// Architecture name from `std::env::consts::ARCH`.
        arch: String,
    },
    /// The pid file references a live nats-server process.
    #[error("managed nats-server is already running (pid {pid}); stop it first or use plain trellis-server with configured external NATS")]
    AlreadyRunning {
        /// Process id read from the pid file.
        pid: i32,
    },
    /// A port needed by the managed server is already in use.
    #[error("port {port} is already in use; stop the existing server or use plain trellis-server with configured external NATS")]
    PortInUse {
        /// Occupied port.
        port: u16,
    },
    /// The managed server did not listen on all ports in time.
    #[error("managed nats-server did not listen on {ports:?} within {timeout:?}; check its dedicated child log")]
    ReadinessTimeout {
        /// Ports that never accepted a TCP connection.
        ports: Vec<u16>,
        /// Timeout that elapsed.
        timeout: Duration,
    },
    /// The managed server exited during startup.
    #[error(
        "managed nats-server exited during startup with status {0}; check its dedicated child log"
    )]
    SpawnFailed(String),
    /// The pid file exists but is not a regular file recording a pid; another owner may
    /// be mid-startup or the path may have been tampered with.
    #[error("pid file {path} exists but is not a regular file recording a pid; inspect and remove it if no trellis-server process is running")]
    PidFileUnparsable {
        /// Pid file path.
        path: PathBuf,
    },
    /// The cache directory is unsafe to use.
    #[error("refusing to use cache directory {path}: {reason}")]
    CacheDirUnsafe {
        /// Cache directory path.
        path: PathBuf,
        /// Why the directory is unsafe.
        reason: String,
    },
    /// A `--local-nats=<PATH>` value is not a usable nats-server executable.
    #[error("refusing to use nats-server binary at {path}: {reason}")]
    InvalidBinaryPath {
        /// Binary path.
        path: PathBuf,
        /// Why the path is unusable.
        reason: String,
    },
    /// Pinned acquisition was requested without an explicit cache directory.
    #[error("pinned nats-server download requires an explicit cache directory")]
    MissingCacheDir,

    /// A required local NATS builder choice was omitted or contradictory.
    #[error("invalid local NATS policy: {0}")]
    InvalidPolicy(String),
    /// Downloaded bytes do not match the pinned sha256.
    #[error("checksum mismatch for nats-server download: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Pinned sha256.
        expected: String,
        /// Computed sha256.
        actual: String,
    },
    /// The pinned manifest has no checksum for this platform asset.
    #[error("no pinned sha256 for asset {asset} in local-nats/nats-binaries.json")]
    MissingSha256 {
        /// Platform asset name.
        asset: String,
    },
    /// The release archive does not contain a regular nats-server binary.
    #[error("nats-server archive {path} did not contain a regular nats-server binary")]
    MissingBinary {
        /// Archive path.
        path: PathBuf,
    },
    /// The nats-server download failed.
    #[error("failed to download nats-server from {url}: {message}")]
    Download {
        /// Download URL.
        url: String,
        /// Underlying failure.
        message: String,
    },
    /// The embedded pinned-binaries manifest is malformed.
    #[error("invalid embedded local-nats/nats-binaries.json: {0}")]
    InvalidPinnedManifest(#[from] serde_json::Error),
    /// Filesystem or process operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Map an OS/architecture pair to the nats-server release asset name (e.g. `linux-amd64`).
pub fn asset_name(os: &str, arch: &str) -> Result<&'static str, LocalNatsError> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("linux-amd64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("macos", "x86_64") => Ok("darwin-amd64"),
        ("macos", "aarch64") => Ok("darwin-arm64"),
        (os, arch) => Err(LocalNatsError::UnsupportedPlatform {
            os: os.to_string(),
            arch: arch.to_string(),
        }),
    }
}

/// Asset name for the current host platform.
pub fn platform() -> Result<&'static str, LocalNatsError> {
    asset_name(std::env::consts::OS, std::env::consts::ARCH)
}

#[derive(Clone, Deserialize)]
struct PinnedNatsServer {
    version: String,
    sha256: HashMap<String, String>,
}

#[derive(Deserialize)]
struct PinnedNatsBinaries {
    #[serde(rename = "nats-server")]
    nats_server: PinnedNatsServer,
}

fn pinned() -> Result<PinnedNatsServer, LocalNatsError> {
    serde_json::from_str::<PinnedNatsBinaries>(NATS_BINARIES_JSON)
        .map(|binaries| binaries.nats_server)
        .map_err(LocalNatsError::from)
}

/// Pinned nats-server version, shared with the TypeScript harness.
pub fn pinned_version() -> Result<String, LocalNatsError> {
    Ok(pinned()?.version)
}

#[cfg(test)]
fn pinned_sha256(asset: &str) -> Result<String, LocalNatsError> {
    pinned()?
        .sha256
        .get(asset)
        .cloned()
        .ok_or(LocalNatsError::MissingSha256 {
            asset: asset.to_string(),
        })
}

/// Official GitHub release URL for the pinned nats-server archive for the given
/// asset, derived from the pinned version (no URL is stored in the pin file).
pub fn download_url(asset: &str) -> Result<String, LocalNatsError> {
    download_url_for_version(&pinned()?.version, asset)
}

/// Official GitHub release URL for the nats-server archive at an explicit version.
fn download_url_for_version(version: &str, asset: &str) -> Result<String, LocalNatsError> {
    let (os, arch) = asset.split_once('-').ok_or(LocalNatsError::MissingSha256 {
        asset: asset.to_string(),
    })?;
    Ok(format!(
        "https://github.com/nats-io/nats-server/releases/download/v{version}/nats-server-v{version}-{os}-{arch}.tar.gz"
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_sha256(expected: &str, bytes: &[u8]) -> Result<(), LocalNatsError> {
    let actual = sha256_hex(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(LocalNatsError::ChecksumMismatch {
            expected: expected.to_string(),
            actual,
        })
    }
}

fn cache_base_dir(home: &Path, macos: bool) -> PathBuf {
    if macos {
        home.join("Library/Caches/trellis")
    } else {
        home.join(".cache/trellis")
    }
}

/// `$TRELLIS_CACHE_DIR` when set, otherwise `~/.cache/trellis` on linux and
/// `~/Library/Caches/trellis` on macOS.
fn default_cache_dir() -> Result<PathBuf, LocalNatsError> {
    if let Some(dir) = std::env::var_os("TRELLIS_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(cache_base_dir(
        &PathBuf::from(home),
        cfg!(target_os = "macos"),
    ))
}

/// Create the cache directory with 0o700 permissions, or reject an existing directory that
/// is a symlink, is not owned by the current user, or grants group/world access.
fn ensure_cache_dir(cache_dir: &Path) -> Result<(), LocalNatsError> {
    match fs::symlink_metadata(cache_dir) {
        Ok(metadata) => validate_cache_dir(cache_dir, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(cache_dir)?;
            set_private_dir_permissions(cache_dir)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn validate_cache_dir(cache_dir: &Path, metadata: &fs::Metadata) -> Result<(), LocalNatsError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if metadata.file_type().is_symlink() {
        return Err(LocalNatsError::CacheDirUnsafe {
            path: cache_dir.to_path_buf(),
            reason: "cache root must not be a symlink".to_string(),
        });
    }
    if !metadata.file_type().is_dir() {
        return Err(LocalNatsError::CacheDirUnsafe {
            path: cache_dir.to_path_buf(),
            reason: "not a directory".to_string(),
        });
    }
    // SAFETY: geteuid never fails.
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid {
        return Err(LocalNatsError::CacheDirUnsafe {
            path: cache_dir.to_path_buf(),
            reason: format!("owned by uid {}, expected {current_uid}", metadata.uid()),
        });
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(LocalNatsError::CacheDirUnsafe {
            path: cache_dir.to_path_buf(),
            reason: format!(
                "permissions {:o} allow group or world access",
                metadata.permissions().mode()
            ),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_cache_dir(_cache_dir: &Path, _metadata: &fs::Metadata) -> Result<(), LocalNatsError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), LocalNatsError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), LocalNatsError> {
    Ok(())
}

#[cfg(unix)]
fn is_executable_mode(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_mode(_metadata: &fs::Metadata) -> bool {
    true
}

/// Managed nats-server binary provider: downloads, verifies, and caches the pinned release.
pub struct NatsServerBinary;

/// Explicit source policy for a managed nats-server binary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NatsBinarySource {
    /// Find `nats-server` on `PATH` without downloading.
    PathLookup,
    /// Validate and use this exact executable without downloading.
    Path(PathBuf),
    /// Download and verify the Trellis-pinned release.
    DownloadPinned,
}

impl NatsServerBinary {
    /// Resolve an explicit binary source. Download mode requires `cache_dir`; all other
    /// modes ignore it and never access the network.
    pub fn resolve(
        source: &NatsBinarySource,
        cache_dir: Option<&Path>,
    ) -> Result<PathBuf, LocalNatsError> {
        match source {
            NatsBinarySource::PathLookup => Self::from_path_lookup(),
            NatsBinarySource::Path(path) => Self::from_path(path),
            NatsBinarySource::DownloadPinned => {
                Self::ensure(Some(cache_dir.ok_or(LocalNatsError::MissingCacheDir)?))
            }
        }
    }

    /// Find and validate `nats-server` on the process `PATH` without downloading.
    pub fn from_path_lookup() -> Result<PathBuf, LocalNatsError> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        Self::from_search_path(&path)
    }

    fn from_search_path(path: &std::ffi::OsStr) -> Result<PathBuf, LocalNatsError> {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(if cfg!(windows) {
                "nats-server.exe"
            } else {
                "nats-server"
            });
            if candidate.is_file() {
                return Self::from_path(&candidate);
            }
        }
        Err(LocalNatsError::InvalidBinaryPath {
            path: PathBuf::from("nats-server"),
            reason: "not found on PATH; install nats-server, use --local-nats=<PATH>, or request --nats-download".to_string(),
        })
    }

    /// Return the path to a verified nats-server binary, downloading and extracting the
    /// pinned release into `cache_dir` on first use. `None` uses `TRELLIS_CACHE_DIR` or the
    /// platform cache directory.
    ///
    /// Cache reuse is cryptographically revalidated on every call: the cached archive must
    /// still match the pinned sha256, and the cached binary must be byte-identical to a
    /// fresh extraction of that verified archive. A corrupt archive is re-downloaded; a
    /// missing, replaced, corrupt, symlinked, or otherwise non-regular binary is
    /// reinstalled from the verified archive (never read through). A valid archive plus
    /// matching binary is reused without downloading.
    fn ensure(cache_dir: Option<&Path>) -> Result<PathBuf, LocalNatsError> {
        Self::ensure_with_pin(cache_dir, &pinned()?)
    }

    /// `ensure` against an explicit pin, so tests can exercise the cache flow hermetically.
    fn ensure_with_pin(
        cache_dir: Option<&Path>,
        pin: &PinnedNatsServer,
    ) -> Result<PathBuf, LocalNatsError> {
        let asset = platform()?;
        let cache_dir = match cache_dir {
            Some(dir) => dir.to_path_buf(),
            None => default_cache_dir()?,
        };
        ensure_cache_dir(&cache_dir)?;
        let binary = cache_dir.join(format!("nats-server-v{}-{asset}", pin.version));
        let archive = cache_dir.join(format!("nats-server-v{}-{asset}.tar.gz", pin.version));
        let expected_sha256 =
            pin.sha256
                .get(asset)
                .cloned()
                .ok_or_else(|| LocalNatsError::MissingSha256 {
                    asset: asset.to_string(),
                })?;
        // Serialize installation per cache dir: concurrent `ensure` calls cannot interfere
        // with each other's temp files or the final rename. The guard is held across the
        // whole download/install critical section.
        let install_lock = install_lock(&cache_dir);
        let _guard = install_lock.lock().expect("install lock poisoned");
        // The verified archive is the root of trust for reuse; a missing or corrupt cached
        // archive is re-downloaded before any binary is trusted or installed.
        if !archive_is_verified(&archive, &expected_sha256)? {
            download_verified_archive(&archive, asset, &expected_sha256, &pin.version)?;
        }
        if let Some(binary) = verified_installed_binary(&binary, &archive)? {
            return Ok(binary);
        }
        let staging = unique_temp_path(
            &cache_dir,
            &format!(".nats-server-v{}-{asset}", pin.version),
        );
        install_binary(&archive, &staging, &binary)?;
        Ok(binary)
    }

    /// Validate `path` as a pre-installed nats-server binary and return the canonical path
    /// to spawn.
    ///
    /// This is an explicit trusted-operator escape hatch: the binary is NOT re-verified
    /// against the Trellis pin or version, so the caller takes responsibility for its
    /// provenance (for example a binary baked into a container image at build time). The
    /// path is canonicalized and must be a regular, executable file owned by root or the
    /// current user, with no group- or world-writable bits on the file or on any parent
    /// directory; symlinks at the path itself are rejected. Spawn the returned canonical
    /// path, never the caller-provided one.
    pub fn from_path(path: &Path) -> Result<PathBuf, LocalNatsError> {
        let invalid = |reason: &str| LocalNatsError::InvalidBinaryPath {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(invalid("does not exist"))
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(invalid("must not be a symlink"));
        }
        if !metadata.file_type().is_file() {
            return Err(invalid("not a regular file"));
        }
        if !is_executable_mode(&metadata) {
            return Err(invalid("not executable"));
        }
        // Resolve symlinked parent components; the leaf was already proven regular.
        let canonical = fs::canonicalize(path)?;
        let canonical_metadata = fs::symlink_metadata(&canonical)?;
        validate_trusted_binary_metadata(&canonical, &canonical_metadata, &invalid)?;
        Ok(canonical)
    }
}

/// Child-output routing for a managed nats-server process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NatsOutput {
    /// Append stdout and stderr to this file, optionally mirroring both to stderr.
    Log {
        /// File receiving both output streams in append mode.
        path: PathBuf,
        /// Also copy both output streams to the parent's stderr.
        mirror: bool,
    },
}

/// Ports for a managed local NATS process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalNatsPorts {
    /// Native NATS client port.
    pub nats: u16,
    /// HTTP monitoring port.
    pub monitor: u16,
    /// Browser websocket port.
    pub websocket: u16,
}

impl Default for LocalNatsPorts {
    fn default() -> Self {
        Self {
            nats: 4222,
            monitor: 8222,
            websocket: 8080,
        }
    }
}

/// Managed local NATS process assembled from explicit builder policy.
pub struct LocalNats {
    server: ManagedNatsServer,
    nats_url: String,
    websocket_url: String,
    log_path: PathBuf,
    _temporary_state: Option<tempfile::TempDir>,
}

impl LocalNats {
    /// Begin an explicit local NATS policy.
    #[must_use]
    pub fn builder() -> LocalNatsBuilder {
        LocalNatsBuilder::default()
    }

    /// Native NATS client URL.
    #[must_use]
    pub fn nats_url(&self) -> &str {
        &self.nats_url
    }

    /// Browser websocket URL.
    #[must_use]
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    /// Dedicated child-output log path.
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Stop and reap the managed child.
    pub fn stop(&mut self) -> Result<(), LocalNatsError> {
        self.server.stop()
    }
}

#[derive(Default)]
enum StatePolicy {
    #[default]
    Missing,
    Path(PathBuf),
    Temporary,
}

#[derive(Default)]
enum PortPolicy {
    #[default]
    Missing,
    Fixed(LocalNatsPorts),
    Ephemeral,
}

/// Builder requiring explicit binary, config source, state, ports, and output policy.
#[derive(Default)]
pub struct LocalNatsBuilder {
    binary: Option<NatsBinarySource>,
    source: Option<PathBuf>,
    state: StatePolicy,
    ports: PortPolicy,
    cache_dir: Option<PathBuf>,
    pid_file: Option<PathBuf>,
    output: Option<NatsOutput>,
}

impl LocalNatsBuilder {
    /// Select where the managed nats-server executable comes from.
    #[must_use]
    pub fn binary(mut self, source: NatsBinarySource) -> Self {
        self.binary = Some(source);
        self
    }

    /// Select the read-only directory containing `nats.conf` and `jwt.conf`.
    #[must_use]
    pub fn source(mut self, path: impl Into<PathBuf>) -> Self {
        self.source = Some(path.into());
        self
    }

    /// Select the mutable NATS state directory.
    #[must_use]
    pub fn state(mut self, path: impl Into<PathBuf>) -> Self {
        self.state = StatePolicy::Path(path.into());
        self
    }

    /// Keep mutable NATS state in a temporary directory owned by the returned guard.
    #[must_use]
    pub fn temporary_state(mut self) -> Self {
        self.state = StatePolicy::Temporary;
        self
    }

    /// Select fixed local ports.
    #[must_use]
    pub fn ports(mut self, ports: LocalNatsPorts) -> Self {
        self.ports = PortPolicy::Fixed(ports);
        self
    }

    /// Reserve nonconflicting loopback ports for this process.
    #[must_use]
    pub fn ephemeral_ports(mut self) -> Self {
        self.ports = PortPolicy::Ephemeral;
        self
    }

    /// Select the cache used only by [`NatsBinarySource::DownloadPinned`].
    #[must_use]
    pub fn cache_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(path.into());
        self
    }

    /// Override the PID file location; otherwise it lives under the state directory.
    #[must_use]
    pub fn pid_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.pid_file = Some(path.into());
        self
    }

    /// Select dedicated child log routing.
    #[must_use]
    pub fn output(mut self, output: NatsOutput) -> Self {
        self.output = Some(output);
        self
    }

    /// Resolve all policy, render host-local config, and start NATS.
    pub fn start(self) -> Result<LocalNats, LocalNatsError> {
        let binary_source = self
            .binary
            .ok_or_else(|| LocalNatsError::InvalidPolicy("binary source is required".into()))?;
        let source = self
            .source
            .ok_or_else(|| LocalNatsError::InvalidPolicy("config source is required".into()))?
            .canonicalize()?;
        let (state, temporary_state) = match self.state {
            StatePolicy::Path(path) => (path, None),
            StatePolicy::Temporary => {
                let temporary = tempfile::tempdir()?;
                (temporary.path().to_path_buf(), Some(temporary))
            }
            StatePolicy::Missing => {
                return Err(LocalNatsError::InvalidPolicy(
                    "state or temporary_state is required".into(),
                ));
            }
        };
        let (ports, reservations) = match self.ports {
            PortPolicy::Fixed(ports) => (ports, Vec::new()),
            PortPolicy::Ephemeral => {
                let listeners = (0..3)
                    .map(|_| TcpListener::bind(("127.0.0.1", 0)))
                    .collect::<io::Result<Vec<_>>>()?;
                let ports = LocalNatsPorts {
                    nats: listeners[0].local_addr()?.port(),
                    monitor: listeners[1].local_addr()?.port(),
                    websocket: listeners[2].local_addr()?.port(),
                };
                (ports, listeners)
            }
            PortPolicy::Missing => {
                return Err(LocalNatsError::InvalidPolicy(
                    "ports or ephemeral_ports is required".into(),
                ));
            }
        };
        let output = self
            .output
            .ok_or_else(|| LocalNatsError::InvalidPolicy("output policy is required".into()))?;
        fs::create_dir_all(state.join("data/jwt"))?;
        let authored_config = fs::read_to_string(source.join("nats.conf"))?;
        let server_name = authored_config
            .lines()
            .find_map(|line| line.trim().strip_prefix("server_name:").map(str::trim))
            .unwrap_or("trellis");
        let jwt_config = state.join("jwt.conf");
        fs::write(
            &jwt_config,
            trellis_bootstrap::render_local_jwt_config(
                &fs::read_to_string(source.join("jwt.conf"))?,
                &state.join("data/jwt").display().to_string(),
            ),
        )?;
        let config = state.join("nats.conf");
        fs::write(
            &config,
            trellis_bootstrap::render_local_nats_config(
                server_name,
                &state.join("data").display().to_string(),
                "./jwt.conf",
                ports.nats,
                ports.websocket,
                ports.monitor,
            ),
        )?;
        let binary = NatsServerBinary::resolve(&binary_source, self.cache_dir.as_deref())?;
        let pid_file = self
            .pid_file
            .unwrap_or_else(|| state.join("nats-server.pid"));
        let log_path = match &output {
            NatsOutput::Log { path, .. } => path.clone(),
        };
        drop(reservations);
        let server = ManagedNatsServer::start(
            &binary,
            &config,
            ports.nats,
            ports.monitor,
            ports.websocket,
            &pid_file,
            &output,
        )?;
        Ok(LocalNats {
            server,
            nats_url: format!("nats://127.0.0.1:{}", ports.nats),
            websocket_url: format!("ws://127.0.0.1:{}", ports.websocket),
            log_path,
            _temporary_state: temporary_state,
        })
    }
}

/// Returns `Ok(Some(path))` when the installed binary is a regular, executable file;
/// `Ok(None)` when absent, not executable, or a symlinked/otherwise non-regular cache
/// entry (reinstall from the verified archive). The entry is never read through: the
/// reinstall replaces the directory entry itself.
fn installed_binary(binary: &Path) -> Result<Option<PathBuf>, LocalNatsError> {
    match fs::symlink_metadata(binary) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                // A symlinked or otherwise non-regular cache entry is stale; replace it
                // from the verified archive like any other missing binary.
                return Ok(None);
            }
            if is_executable_mode(&metadata) {
                Ok(Some(binary.to_path_buf()))
            } else {
                // A regular but non-executable file is a stale partial install; replace it.
                Ok(None)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Returns true when the cached archive exists and its sha256 matches the pin. A cached
/// archive that no longer matches is removed so the caller re-downloads it from the pin.
fn archive_is_verified(archive: &Path, expected_sha256: &str) -> Result<bool, LocalNatsError> {
    match fs::read(archive) {
        Ok(bytes) => {
            if sha256_hex(&bytes) == expected_sha256 {
                Ok(true)
            } else {
                let _ = fs::remove_file(archive);
                Ok(false)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Download the pinned archive, verify its sha256, and atomically rename it into place.
/// Every failure path removes the temp file.
fn download_verified_archive(
    archive: &Path,
    asset: &str,
    expected_sha256: &str,
    version: &str,
) -> Result<(), LocalNatsError> {
    let temp_archive = unique_temp_path(
        archive.parent().expect("archive path has a parent"),
        archive
            .file_name()
            .expect("archive path has a file name")
            .to_str()
            .expect("archive file name is UTF-8"),
    );
    let fetch = (|| -> Result<(), LocalNatsError> {
        let bytes = download_bytes(&agent(), &download_url_for_version(version, asset)?)?;
        verify_and_write(&bytes, expected_sha256, &temp_archive)?;
        Ok(())
    })();
    if let Err(error) = fetch {
        let _ = fs::remove_file(&temp_archive);
        return Err(error);
    }
    if let Err(error) = rename_over(&temp_archive, archive) {
        let _ = fs::remove_file(&temp_archive);
        return Err(error);
    }
    Ok(())
}

/// Returns `Ok(Some(path))` only when the cached binary is a regular executable file
/// byte-identical to a fresh extraction of the verified `archive`. A missing,
/// non-executable, symlinked, otherwise non-regular, or replaced binary returns
/// `Ok(None)` so the caller reinstalls it from the archive.
fn verified_installed_binary(
    binary: &Path,
    archive: &Path,
) -> Result<Option<PathBuf>, LocalNatsError> {
    let Some(binary_path) = installed_binary(binary)? else {
        return Ok(None);
    };
    let temp_dir = unique_temp_path(
        binary.parent().expect("binary path has a parent"),
        ".nats-server-verify",
    );
    fs::create_dir_all(&temp_dir)?;
    let fresh = temp_dir.join("nats-server");
    let result = extract_binary(archive, &fresh).and_then(|()| {
        if fs::read(&fresh)? == fs::read(&binary_path)? {
            Ok(Some(binary_path))
        } else {
            Ok(None)
        }
    });
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

/// Reject a trusted `--local-nats=<PATH>` file that is not owned by root or the current user or
/// that has group/world-writable bits on the file or on any parent directory component.
///
/// A parent directory is tolerated when it is group/world-writable only with the sticky
/// bit set (for example `/tmp`): the sticky bit prevents other users from replacing files
/// they do not own, so an operator-owned binary there cannot be swapped by a less
/// privileged user.
#[cfg(unix)]
fn validate_trusted_binary_metadata(
    canonical: &Path,
    metadata: &fs::Metadata,
    invalid: &dyn Fn(&str) -> LocalNatsError,
) -> Result<(), LocalNatsError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    // SAFETY: geteuid never fails.
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != 0 && metadata.uid() != current_uid {
        return Err(invalid(
            "must be owned by root or the current user (trusted operator input)",
        ));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(invalid("file must not be group or world writable"));
    }
    let mut ancestor = canonical.parent();
    while let Some(dir) = ancestor {
        let dir_metadata = fs::symlink_metadata(dir)?;
        let mode = dir_metadata.permissions().mode();
        if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            return Err(invalid(&format!(
                "parent directory {} must not be group or world writable",
                dir.display()
            )));
        }
        ancestor = dir.parent();
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_trusted_binary_metadata(
    _canonical: &Path,
    _metadata: &fs::Metadata,
    _invalid: &dyn Fn(&str) -> LocalNatsError,
) -> Result<(), LocalNatsError> {
    Ok(())
}

/// Process-local lock serializing installs into `cache_dir`.
fn install_lock(cache_dir: &Path) -> Arc<Mutex<()>> {
    let mut locks = INSTALL_LOCKS
        .lock()
        .expect("install lock registry poisoned");
    locks
        .get_or_insert_with(HashMap::new)
        .entry(cache_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Unique temp path per call within this process.
fn unique_temp_path(cache_dir: &Path, label: &str) -> PathBuf {
    cache_dir.join(format!(
        "{label}.tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// ureq agent with explicit connect, read, and overall timeouts for the release download.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .timeout_connect(Some(DOWNLOAD_CONNECT_TIMEOUT))
        .timeout_recv_body(Some(DOWNLOAD_TIMEOUT))
        .build()
        .new_agent()
}

fn download_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, LocalNatsError> {
    let response = agent
        .get(url)
        .call()
        .map_err(|error| LocalNatsError::Download {
            url: url.to_string(),
            message: error.to_string(),
        })?;
    #[cfg(test)]
    DOWNLOAD_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    io::copy(&mut reader, &mut bytes).map_err(|error| LocalNatsError::Download {
        url: url.to_string(),
        message: error.to_string(),
    })?;
    Ok(bytes)
}

/// Write `bytes` to `temp_path` only after the checksum matches, so a corrupt download
/// never reaches the final archive path.
fn verify_and_write(
    bytes: &[u8],
    expected_sha256: &str,
    temp_path: &Path,
) -> Result<(), LocalNatsError> {
    verify_sha256(expected_sha256, bytes)?;
    fs::write(temp_path, bytes)?;
    Ok(())
}

/// Extract the pinned binary from `archive` into a staging dir and rename it into place.
///
/// The staging dir is removed after a successful rename and on extraction failure, so a
/// failed install never leaves a partially extracted directory behind.
fn install_binary(archive: &Path, staging_dir: &Path, binary: &Path) -> Result<(), LocalNatsError> {
    fs::create_dir_all(staging_dir)?;
    let staged_binary = staging_dir.join("nats-server");
    if let Err(error) = extract_binary(archive, &staged_binary) {
        let _ = fs::remove_dir_all(staging_dir);
        return Err(error);
    }
    // A stale non-regular entry at the binary path (symlink, directory, ...) cannot be
    // renamed over; remove the entry itself first, never reading through it.
    remove_non_regular_entry(binary)?;
    let rename = rename_over(&staged_binary, binary);
    let _ = fs::remove_dir_all(staging_dir);
    rename
}

/// Removes a non-regular entry at `path` (a symlink or directory) so an install rename
/// can replace it. No-follow: the entry itself is unlinked, never read through.
fn remove_non_regular_entry(path: &Path) -> Result<(), LocalNatsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path)?;
            Ok(())
        }
        Ok(_) => {
            fs::remove_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Extract the single regular `nats-server` entry from a release archive into `destination`.
///
/// Symlink and hardlink entries named `nats-server` are ignored; the unpacked file is
/// re-verified as a regular file before returning.
fn extract_binary(archive: &Path, destination: &Path) -> Result<(), LocalNatsError> {
    let decoder = GzDecoder::new(fs::File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let is_binary_entry = entry.header().entry_type().is_file()
            && entry
                .path()?
                .file_name()
                .is_some_and(|name| name == "nats-server");
        if !is_binary_entry {
            continue;
        }
        // Never unpack onto a pre-existing symlink left in the staging dir.
        let _ = fs::remove_file(destination);
        entry.unpack(destination)?;
        let metadata = fs::symlink_metadata(destination)?;
        if !metadata.file_type().is_file() {
            return Err(LocalNatsError::MissingBinary {
                path: archive.to_path_buf(),
            });
        }
        set_executable(destination)?;
        return Ok(());
    }
    Err(LocalNatsError::MissingBinary {
        path: archive.to_path_buf(),
    })
}

fn rename_over(from: &Path, to: &Path) -> Result<(), LocalNatsError> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(to);
            fs::rename(from, to)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), LocalNatsError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), LocalNatsError> {
    Ok(())
}

/// Acquire `pid_file` as an exclusive ownership lock and return the write handle.
///
/// The file is created with exclusive create; the returned handle is the caller's proof of
/// ownership and the pid is written through it (never by re-opening the path). An existing
/// file is inspected through a no-follow read: when it records a live process that is
/// verifiably the managed nats-server binary, startup fails with
/// [`LocalNatsError::AlreadyRunning`]; a stale pid (dead, or a live process that is not our
/// managed binary) is removed before retrying once. An existing non-regular or unparsable
/// file is treated as busy and fails with [`LocalNatsError::PidFileUnparsable`].
fn acquire_pid_file(pid_file: &Path, binary: &Path) -> Result<fs::File, LocalNatsError> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pid_file)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => match read_pid_file(pid_file)
        {
            Ok(Some(pid)) if process_is_managed_nats(pid, binary) => {
                Err(LocalNatsError::AlreadyRunning { pid })
            }
            // Stale pid: remove it and retry the exclusive create once.
            Ok(Some(_)) => {
                fs::remove_file(pid_file)?;
                Ok(fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(pid_file)?)
            }
            // The file vanished between the failed create and the read; retry once.
            Ok(None) => Ok(fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(pid_file)?),
            Err(error) => Err(error),
        },
        Err(error) => Err(error.into()),
    }
}

/// Reads a pid from `path`, no-follow: a symlink, FIFO, or other non-regular file at the
/// path is rejected without being read (a FIFO would block), and `Ok(None)` means the path
/// is absent.
fn read_pid_file(path: &Path) -> Result<Option<i32>, LocalNatsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => Err(LocalNatsError::PidFileUnparsable {
            path: path.to_path_buf(),
        }),
        Ok(_) => {
            let contents = fs::read_to_string(path)?;
            let pid =
                contents
                    .trim()
                    .parse::<i32>()
                    .map_err(|_| LocalNatsError::PidFileUnparsable {
                        path: path.to_path_buf(),
                    })?;
            Ok(Some(pid))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Writes the pid through the exclusive lock handle from [`acquire_pid_file`], so a symlink
/// or FIFO planted at the path afterwards is never followed.
fn write_pid(lock: &mut fs::File, pid: i32) -> Result<(), LocalNatsError> {
    use std::io::Write as _;
    lock.write_all(format!("{pid}\n").as_bytes())?;
    Ok(())
}

/// Verifies the pid file at `path` is still a regular file recording `pid`; rejects a path
/// replaced by a symlink, FIFO, or foreign content between the exclusive create and now.
fn verify_owned_pid_file(path: &Path, pid: i32) -> Result<(), LocalNatsError> {
    match read_pid_file(path) {
        Ok(Some(recorded)) if recorded == pid => Ok(()),
        Ok(_) => Err(LocalNatsError::PidFileUnparsable {
            path: path.to_path_buf(),
        }),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 is an existence probe only; no signal is delivered.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    false
}

/// Whether `pid` is a live managed nats-server.
///
/// On linux the process identity is verified primarily through `/proc/<pid>/exe`: when it
/// is readable, its canonicalized path must equal the canonicalized path of the binary this
/// process manages (the exact path passed to [`ManagedNatsServer::start`], which may be a
/// `--local-nats=<PATH>` escape-hatch path whose basename is not `nats-server`). A recycled pid
/// naming an unrelated process is treated as stale instead of blocking startup. Only when
/// `/proc/<pid>/exe` is unreadable (for example a zombie or a foreign pid namespace) does
/// the gate fall back to the comm-prefix check (`/proc/<pid>/comm` starts with
/// `nats-server`; comm is truncated to 15 chars, so the versioned binary appears as
/// `nats-server-v2.`); that fallback is liveness-only and could treat a recycled pid
/// naming an unrelated nats-server as ours, which conservatively blocks startup.
#[cfg(target_os = "linux")]
fn process_is_managed_nats(pid: i32, binary: &Path) -> bool {
    if !process_alive(pid) {
        return false;
    }
    match fs::canonicalize(format!("/proc/{pid}/exe")) {
        // The managed binary path must resolve for the match to count; an exe that
        // resolves to anything else is stale, regardless of its comm.
        Ok(actual) => fs::canonicalize(binary).is_ok_and(|expected| actual == expected),
        // Unreadable `/proc/<pid>/exe`: fall back to the comm-prefix gate (liveness-only).
        Err(_) => fs::read_to_string(format!("/proc/{pid}/comm"))
            .is_ok_and(|comm| comm.trim().starts_with("nats-server")),
    }
}

/// Whether `pid` is a live managed nats-server.
///
/// Non-linux hosts cannot verify the process identity, so liveness is the only
/// signal; a recycled pid could block startup until the unrelated process exits.
#[cfg(not(target_os = "linux"))]
fn process_is_managed_nats(pid: i32, _binary: &Path) -> bool {
    process_alive(pid)
}

/// Poll until every port accepts TCP connections or `stopped` reports an exited child.
fn wait_for_readiness<F>(
    stopped: &mut F,
    ports: &[u16],
    timeout: Duration,
    interval: Duration,
) -> Result<(), LocalNatsError>
where
    F: FnMut() -> Result<Option<ExitStatus>, LocalNatsError>,
{
    wait_for_readiness_with(stopped, ports, timeout, interval, |port| {
        TcpStream::connect(("127.0.0.1", *port)).is_ok()
    })
}

/// Poll until `probe` reports every port ready or `stopped` reports an exited child.
///
/// Private test seam for [`wait_for_readiness`]: production passes the real TCP-connect
/// probe; unit tests inject deterministic probes so no sockets are needed.
fn wait_for_readiness_with<F>(
    stopped: &mut F,
    ports: &[u16],
    timeout: Duration,
    interval: Duration,
    probe: impl Fn(&u16) -> bool,
) -> Result<(), LocalNatsError>
where
    F: FnMut() -> Result<Option<ExitStatus>, LocalNatsError>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = stopped()? {
            return Err(LocalNatsError::SpawnFailed(status.to_string()));
        }
        if ports.iter().all(&probe) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(LocalNatsError::ReadinessTimeout {
                ports: ports.to_vec(),
                timeout,
            });
        }
        std::thread::sleep(interval);
    }
}

/// Guard owning a spawned managed nats-server process.
///
/// The server is stopped on [`stop`](Self::stop) and, best effort, when the guard is dropped.
pub struct ManagedNatsServer {
    child: Option<Child>,
    pid_file: PathBuf,
    nats_port: u16,
    /// Pid this guard wrote to `pid_file`; used to verify ownership before unlinking.
    pid: i32,
    output_threads: Vec<JoinHandle<io::Result<()>>>,
}

impl ManagedNatsServer {
    /// Spawn `nats-server -c <config_path>` and wait until `nats_port`, `http_port`, and
    /// `ws_port` accept TCP connections (30 second timeout, 100 ms polling). The child's
    /// output follows the explicit `output` policy.
    ///
    /// The pid file is acquired as an exclusive lock before spawning. A pid file recording a
    /// live nats-server fails with [`LocalNatsError::AlreadyRunning`]; a stale pid file is
    /// removed and startup proceeds. A port that is already listening fails with
    /// [`LocalNatsError::PortInUse`]. Every failure after spawn kills and waits for the child
    /// and removes the pid file before returning.
    pub fn start(
        binary: &Path,
        config_path: &Path,
        nats_port: u16,
        http_port: u16,
        ws_port: u16,
        pid_file: &Path,
        output: &NatsOutput,
    ) -> Result<Self, LocalNatsError> {
        let mut pid_lock = acquire_pid_file(pid_file, binary)?;
        for port in [nats_port, http_port, ws_port] {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                // The pid file was created by `acquire_pid_file` in this call; remove it.
                let _ = fs::remove_file(pid_file);
                return Err(LocalNatsError::PortInUse { port });
            }
        }
        let NatsOutput::Log { path, mirror } = output;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log = OpenOptions::new().create(true).append(true).open(path)?;
        let mut command = Command::new(binary);
        command.arg("-c").arg(config_path);
        if *mirror {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            command
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log.try_clone()?));
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(pid_file);
                return Err(error.into());
            }
        };
        let pid = child.id() as i32;
        let mut server = Self {
            child: Some(child),
            pid_file: pid_file.to_path_buf(),
            nats_port,
            pid,
            output_threads: Vec::new(),
        };
        if *mirror {
            let stdout = server
                .child
                .as_mut()
                .expect("managed child is present")
                .stdout
                .take();
            let stderr = server
                .child
                .as_mut()
                .expect("managed child is present")
                .stderr
                .take();
            if let Some(stdout) = stdout {
                server
                    .output_threads
                    .push(forward_output(stdout, log.try_clone()?));
            }
            if let Some(stderr) = stderr {
                server
                    .output_threads
                    .push(forward_output(stderr, log.try_clone()?));
            }
        }
        // The pid is written through the exclusive lock handle, never by re-opening the
        // path, so a symlink or FIFO planted between acquire and write is not followed.
        if let Err(error) = write_pid(&mut pid_lock, pid) {
            let _ = server.stop();
            let _ = fs::remove_file(pid_file);
            return Err(error);
        }
        if let Err(error) = verify_owned_pid_file(pid_file, pid) {
            let _ = server.stop();
            return Err(error);
        }
        let readiness = wait_for_readiness(
            &mut || {
                server
                    .child
                    .as_mut()
                    .expect("managed child is present")
                    .try_wait()
                    .map_err(LocalNatsError::from)
            },
            &[nats_port, http_port, ws_port],
            READINESS_TIMEOUT,
            POLL_INTERVAL,
        );
        if let Err(error) = readiness {
            let _ = server.stop();
            return Err(error);
        }
        Ok(server)
    }

    /// NATS client URL for the managed server.
    #[must_use]
    pub fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.nats_port)
    }

    /// Process id of the managed server.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("managed child is present").id()
    }

    /// Stop the managed server: SIGTERM, wait up to 10 seconds, then kill. Removes the pid
    /// file only when it still records the pid this guard spawned. Never signals a process
    /// this guard did not spawn. Idempotent.
    pub fn stop(&mut self) -> Result<(), LocalNatsError> {
        let Some(child) = self.child.as_mut() else {
            self.remove_owned_pid_file();
            return Ok(());
        };
        if child.try_wait()?.is_none() {
            terminate(child);
            let deadline = Instant::now() + STOP_TIMEOUT;
            while child.try_wait()?.is_none() {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
        self.child = None;
        for thread in self.output_threads.drain(..) {
            thread
                .join()
                .map_err(|_| io::Error::other("nats-server output thread panicked"))??;
        }
        self.remove_owned_pid_file();
        Ok(())
    }

    /// Unlink the pid file only when it is still a regular file recording the pid this
    /// guard wrote; a replaced or symlinked pid file belongs to another owner and is kept.
    fn remove_owned_pid_file(&self) {
        let owned = fs::symlink_metadata(&self.pid_file)
            .is_ok_and(|metadata| metadata.file_type().is_file())
            && fs::read_to_string(&self.pid_file)
                .is_ok_and(|contents| contents.trim() == self.pid.to_string());
        if owned {
            let _ = fs::remove_file(&self.pid_file);
        }
    }
}

fn forward_output(
    mut reader: impl io::Read + Send + 'static,
    mut destination: impl io::Write + Send + 'static,
) -> JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(());
            }
            destination.write_all(&buffer[..read])?;
            io::stderr().write_all(&buffer[..read])?;
        }
    })
}

impl Drop for ManagedNatsServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(unix)]
fn terminate(child: &mut Child) {
    // SAFETY: `child.id()` is the pid of a live child spawned by this process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut Child) {
    let _ = child.kill();
}

#[cfg(test)]
mod tests;
