//! Private sandbox directories, child environments, and automatic port leases.
//!
//! Port assignment is fully automatic: each startup attempt binds four
//! `127.0.0.1:0` listeners and lets the kernel choose the ports, holds all four
//! until immediately before the server spawn, and then closes them in one
//! narrow step. No lock files, fixed ranges, PID-derived ports, or global
//! mutex are used, so independent runtimes and processes never coordinate.

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};

/// Ownership marker filename written at the sandbox root.
const OWNER_MARKER: &str = ".trellis-test-owner";
/// Marker format version, bumped when the marker layout changes.
const OWNER_MARKER_VERSION: &str = "1";

/// How the sandbox directory is treated after the runtime stops.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkdirRetention {
    /// Keep the directory only when the runtime fails or panics.
    OnFailure,
    /// Always keep the directory.
    Always,
    /// Remove the directory even on failure.
    Never,
}

/// Returns an error when the host platform cannot run the harness.
pub(crate) fn ensure_supported_platform() -> Result<(), TrellisTestError> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(TrellisTestError::new(
            TrellisTestErrorKind::UnsupportedPlatform,
            TrellisTestStage::Validation,
            "trellis-test supports Linux only",
        ))
    }
}

/// The four loopback ports reserved for one startup attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PortSet {
    /// Trellis HTTP listener.
    pub http: u16,
    /// Managed NATS native listener.
    pub nats: u16,
    /// Managed NATS monitoring listener.
    pub monitor: u16,
    /// Managed NATS WebSocket listener.
    pub websocket: u16,
}

impl PortSet {
    /// All four ports in a stable order.
    pub(crate) fn all(&self) -> [u16; 4] {
        [self.http, self.nats, self.monitor, self.websocket]
    }
}

/// Four live kernel-assigned loopback reservations.
pub(crate) struct PortLease {
    http: TcpListener,
    nats: TcpListener,
    monitor: TcpListener,
    websocket: TcpListener,
}

impl PortLease {
    /// Reserves four distinct loopback ports, holding every listener open.
    pub(crate) fn reserve() -> Result<Self, TrellisTestError> {
        let bind = |role: &str| -> Result<TcpListener, TrellisTestError> {
            TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)).map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::PortAllocation,
                    TrellisTestStage::PortAllocation,
                    format!("reserving the {role} loopback port: {error}"),
                )
            })
        };
        Ok(Self {
            http: bind("HTTP")?,
            nats: bind("NATS")?,
            monitor: bind("monitor")?,
            websocket: bind("WebSocket")?,
        })
    }

    /// The selected ports.
    pub(crate) fn ports(&self) -> Result<PortSet, TrellisTestError> {
        let port_of = |listener: &TcpListener, role: &str| -> Result<u16, TrellisTestError> {
            listener
                .local_addr()
                .map(|addr| addr.port())
                .map_err(|error| {
                    TrellisTestError::new(
                        TrellisTestErrorKind::PortAllocation,
                        TrellisTestStage::PortAllocation,
                        format!("reading the reserved {role} port: {error}"),
                    )
                })
        };
        Ok(PortSet {
            http: port_of(&self.http, "HTTP")?,
            nats: port_of(&self.nats, "NATS")?,
            monitor: port_of(&self.monitor, "monitor")?,
            websocket: port_of(&self.websocket, "WebSocket")?,
        })
    }

    /// Closes every reservation in one narrow step just before the server spawn.
    pub(crate) fn release_for_spawn(self) {
        drop(self.http);
        drop(self.nats);
        drop(self.monitor);
        drop(self.websocket);
    }
}

/// Classifies whether `diagnostics` describes a bind race on one of `ports`.
///
/// Only the managed-NATS `port <selected> is already in use` line and the
/// runtime HTTP listener's own bind failure for the selected HTTP endpoint
/// count; any other failure must not be retried.
pub(crate) fn is_port_conflict(diagnostics: &str, ports: &PortSet) -> bool {
    let lowered = diagnostics.to_ascii_lowercase();
    // Managed NATS names the exact selected port it could not bind.
    if ports
        .all()
        .iter()
        .any(|port| lowered.contains(&format!("port {port} is already in use")))
    {
        return true;
    }
    // The runtime HTTP listener reports its selected endpoint with an OS bind error.
    lowered.contains(&format!(
        "failed to bind runtime http listener at 127.0.0.1:{}",
        ports.http
    )) && lowered.contains("address already in use")
}

/// A private per-attempt sandbox directory.
pub(crate) struct Sandbox {
    root: PathBuf,
    ownership_id: String,
    retention: WorkdirRetention,
    failed: bool,
    cleaned: bool,
}

impl Sandbox {
    /// Creates a new random sandbox beneath `parent`.
    pub(crate) fn create(
        parent: &Path,
        retention: WorkdirRetention,
    ) -> Result<Self, TrellisTestError> {
        let ownership_id = ulid::Ulid::new().to_string();
        let root = parent.join(format!("trellis-test-{ownership_id}"));
        create_private_dir(&root)?;
        for child in [
            "home", "config", "data", "state", "cache", "runtime", "logs",
        ] {
            create_private_dir(&root.join(child))?;
        }
        std::fs::write(
            root.join(OWNER_MARKER),
            format!("{OWNER_MARKER_VERSION} {ownership_id}\n"),
        )?;
        Ok(Self {
            root,
            ownership_id,
            retention,
            failed: false,
            cleaned: false,
        })
    }

    /// Sandbox root directory.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding generated configuration files.
    pub(crate) fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    /// Marks the attempt as failed so `OnFailure` retains the sandbox.
    pub(crate) fn mark_failed(&mut self) {
        self.failed = true;
    }

    /// Applies the sandbox environment to a child command without mutating the parent.
    pub(crate) fn apply_child_env(&self, command: &mut std::process::Command, path: &OsString) {
        command.env_clear();
        command.env("PATH", path);
        let home = self.root.join("home");
        let set = |command: &mut std::process::Command, key: &str, value: PathBuf| {
            command.env(key, value);
        };
        set(command, "HOME", home.clone());
        set(command, "XDG_CONFIG_HOME", self.root.join("config"));
        set(command, "XDG_DATA_HOME", self.root.join("data"));
        set(command, "XDG_STATE_HOME", self.root.join("state"));
        set(command, "XDG_CACHE_HOME", self.root.join("cache"));
        set(command, "XDG_RUNTIME_DIR", self.root.join("runtime"));
        set(command, "TMPDIR", self.root.join("state"));
        set(command, "TRELLIS_CACHE_DIR", self.root.join("cache"));
        command.env("NO_COLOR", "1");
        command.env("TOKIO_WORKER_THREADS", "2");
    }

    /// Removes the sandbox when policy permits, reporting any cleanup failure.
    ///
    /// A preexisting sibling is never touched: only the directory whose marker
    /// records this sandbox's ownership id is removed.
    pub(crate) fn cleanup(&mut self) -> Option<TrellisTestError> {
        if self.cleaned {
            return None;
        }
        self.cleaned = true;
        let keep = match self.retention {
            WorkdirRetention::Always => true,
            WorkdirRetention::OnFailure => self.failed,
            WorkdirRetention::Never => false,
        };
        if keep {
            return None;
        }
        let marker = self.root.join(OWNER_MARKER);
        let marker_ok = std::fs::read_to_string(&marker)
            .map(|contents| {
                contents.trim() == format!("{OWNER_MARKER_VERSION} {}", self.ownership_id)
            })
            .unwrap_or(false);
        if !marker_ok {
            return Some(TrellisTestError::new(
                TrellisTestErrorKind::Cleanup,
                TrellisTestStage::Shutdown,
                "sandbox ownership marker missing or mismatched; retaining directory",
            ));
        }
        std::fs::remove_dir_all(&self.root).err().map(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Cleanup,
                TrellisTestStage::Shutdown,
                format!("removing the sandbox: {error}"),
            )
        })
    }
}

/// Removes stale harness-owned sandbox directories beneath `parent`.
///
/// Only directories named `trellis-test-<id>` whose ownership marker records the
/// same `<id>` are removed, so unrelated files and other tools' directories are
/// never touched. A directory whose most recent modification is within
/// `older_than` is left alone, so a sweep cannot delete a concurrently running
/// test's sandbox; callers should still run it when no harness tests are active.
///
/// This is the reclaim path for sandboxes a killed or aborted process could not
/// remove; [`TrellisTestRuntime::shutdown`] is the per-runtime guarantee.
///
/// # Errors
///
/// Returns [`TrellisTestError`] when `parent` cannot be read or `older_than` is
/// too large to subtract from the current time.
pub fn remove_retained_workdirs(
    parent: &Path,
    older_than: std::time::Duration,
) -> Result<usize, TrellisTestError> {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(older_than)
        .ok_or_else(|| {
            TrellisTestError::new(
                TrellisTestErrorKind::InvalidConfiguration,
                TrellisTestStage::Validation,
                "the retention age is too large to subtract from the current time",
            )
        })?;
    let entries = std::fs::read_dir(parent).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Io,
            TrellisTestStage::Shutdown,
            format!("reading {}: {error}", parent.display()),
        )
    })?;
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(ownership_id) = name
            .to_str()
            .and_then(|name| name.strip_prefix("trellis-test-"))
        else {
            continue;
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let marker = std::fs::read_to_string(path.join(OWNER_MARKER)).unwrap_or_default();
        if marker.trim() != format!("{OWNER_MARKER_VERSION} {ownership_id}") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if stale && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Creates a directory (and parents), readable only by the owning user on Unix.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), TrellisTestError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Io,
            TrellisTestStage::ConfigGeneration,
            format!("creating {}: {error}", path.display()),
        )
    })
}

/// Rejects a sandbox path that cannot be represented in UTF-8 configuration.
pub(crate) fn require_utf8_path(path: &Path, role: &str) -> Result<String, TrellisTestError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        TrellisTestError::new(
            TrellisTestErrorKind::InvalidConfiguration,
            TrellisTestStage::ConfigGeneration,
            format!("the {role} path is not representable as UTF-8"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_ports() -> PortSet {
        PortSet {
            http: 53001,
            nats: 53002,
            monitor: 53003,
            websocket: 53004,
        }
    }

    #[test]
    fn port_conflict_matches_only_selected_ports() {
        let ports = selected_ports();
        assert!(is_port_conflict(
            "failed to bind runtime HTTP listener at 127.0.0.1:53001: Address already in use (os error 98)",
            &ports
        ));
        assert!(is_port_conflict("port 53002 is already in use", &ports));
    }

    #[test]
    fn port_conflict_ignores_unrelated_bind_failures() {
        let ports = selected_ports();
        // An unrelated socket's OS error must not be retried.
        assert!(!is_port_conflict(
            "connecting to another service: Address already in use (os error 98)",
            &ports
        ));
        // A different port on our own listener is not one of the selected ports.
        assert!(!is_port_conflict("port 59999 is already in use", &ports));
        assert!(!is_port_conflict(
            "failed to bind runtime HTTP listener at 127.0.0.1:59999: Address already in use (os error 98)",
            &ports
        ));
        // The HTTP signature without the OS bind error is not a conflict.
        assert!(!is_port_conflict(
            "failed to bind runtime HTTP listener at 127.0.0.1:53001: permission denied",
            &ports
        ));
        // Application text, authentication errors, and malformed config never retry.
        assert!(!is_port_conflict(
            "address already in use is a common phrase",
            &ports
        ));
        assert!(!is_port_conflict(
            "authentication failed: invalid session proof",
            &ports
        ));
        assert!(!is_port_conflict(
            "invalid configuration: missing field `http.port`",
            &ports
        ));
    }

    #[test]
    fn remove_retained_workdirs_only_removes_stale_owned_dirs() {
        use std::time::Duration;
        let parent = tempfile::tempdir().expect("temp dir");
        let owned = parent
            .path()
            .join("trellis-test-01ARZ3NDEKTSV4RRFFQ69G5FAV");
        std::fs::create_dir_all(&owned).expect("create owned");
        std::fs::write(
            owned.join(OWNER_MARKER),
            format!("{OWNER_MARKER_VERSION} 01ARZ3NDEKTSV4RRFFQ69G5FAV\n"),
        )
        .expect("write marker");
        // A directory without a matching ownership marker is never touched.
        let unrelated = parent.path().join("trellis-test-not-owned");
        std::fs::create_dir_all(&unrelated).expect("create unrelated");
        // A marker that does not match its directory name is never touched.
        let mismatched = parent
            .path()
            .join("trellis-test-01ARZ3NDEKTSV4RRFFQ69G5FAW");
        std::fs::create_dir_all(&mismatched).expect("create mismatched");
        std::fs::write(
            mismatched.join(OWNER_MARKER),
            format!("{OWNER_MARKER_VERSION} 01ARZ3NDEKTSV4RRFFQ69G5FAV\n"),
        )
        .expect("write marker");

        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(
            remove_retained_workdirs(parent.path(), Duration::ZERO).expect("sweep"),
            1
        );
        assert!(!owned.exists());
        assert!(unrelated.exists());
        assert!(mismatched.exists());

        // A freshly created owned directory is left alone by an age-gated sweep.
        let fresh = parent
            .path()
            .join("trellis-test-01ARZ3NDEKTSV4RRFFQ69G5FAX");
        std::fs::create_dir_all(&fresh).expect("create fresh");
        std::fs::write(
            fresh.join(OWNER_MARKER),
            format!("{OWNER_MARKER_VERSION} 01ARZ3NDEKTSV4RRFFQ69G5FAX\n"),
        )
        .expect("write marker");
        assert_eq!(
            remove_retained_workdirs(parent.path(), Duration::from_secs(3600)).expect("sweep"),
            0
        );
        assert!(fresh.exists());
    }

    #[test]
    fn port_lease_reserves_four_distinct_loopback_ports() {
        let lease = PortLease::reserve().expect("reserve loopback ports");
        let set = lease.ports().expect("read reserved ports");
        let unique: std::collections::HashSet<u16> = set.all().into_iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn never_retention_removes_only_the_owned_sandbox() {
        let parent = tempfile::tempdir().expect("temp dir");
        let sibling = parent.path().join("preexisting-sibling");
        std::fs::create_dir_all(&sibling).expect("create sibling");
        let mut sandbox =
            Sandbox::create(parent.path(), WorkdirRetention::Never).expect("create sandbox");
        let root = sandbox.root().to_path_buf();
        assert!(root.is_dir());
        assert!(sandbox.cleanup().is_none());
        assert!(!root.exists());
        assert!(
            sibling.is_dir(),
            "a preexisting sibling must survive cleanup"
        );
    }

    #[test]
    fn always_retention_keeps_the_sandbox() {
        let parent = tempfile::tempdir().expect("temp dir");
        let mut sandbox =
            Sandbox::create(parent.path(), WorkdirRetention::Always).expect("create sandbox");
        let root = sandbox.root().to_path_buf();
        assert!(sandbox.cleanup().is_none());
        assert!(root.is_dir());
    }

    #[test]
    fn on_failure_retention_keeps_a_failed_sandbox_and_removes_a_clean_one() {
        let parent = tempfile::tempdir().expect("temp dir");

        let mut failed =
            Sandbox::create(parent.path(), WorkdirRetention::OnFailure).expect("create sandbox");
        let failed_root = failed.root().to_path_buf();
        failed.mark_failed();
        assert!(failed.cleanup().is_none());
        assert!(failed_root.is_dir());

        let mut clean =
            Sandbox::create(parent.path(), WorkdirRetention::OnFailure).expect("create sandbox");
        let clean_root = clean.root().to_path_buf();
        assert!(clean.cleanup().is_none());
        assert!(!clean_root.exists());
    }
}
