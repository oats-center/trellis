//! Private sandbox directories, child environments, and automatic port leases.
//!
//! Port assignment is fully automatic: each startup attempt binds four
//! `127.0.0.1:0` listeners and lets the kernel choose the ports, holds all four
//! until immediately before the server spawn, and then closes them in one
//! narrow step. No lock files, fixed ranges, PID-derived ports, or global
//! mutex are used, so independent runtimes and processes never coordinate.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};

/// Ownership marker filename written at the sandbox root.
const OWNER_MARKER: &str = ".trellis-testkit-owner";
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
            "trellis-testkit supports Linux only",
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

    /// Closes every reservation in one narrow step just before broker startup.
    pub(crate) fn release_for_spawn(self) {
        drop(self.http);
        drop(self.nats);
        drop(self.monitor);
        drop(self.websocket);
    }
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
        let root = parent.join(format!("trellis-testkit-{ownership_id}"));
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
/// Only directories named `trellis-testkit-<id>` whose ownership marker records the
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
            .and_then(|name| name.strip_prefix("trellis-testkit-"))
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

    #[test]
    fn remove_retained_workdirs_only_removes_stale_owned_dirs() {
        use std::time::Duration;
        let parent = tempfile::tempdir().expect("temp dir");
        let owned = parent
            .path()
            .join("trellis-testkit-01ARZ3NDEKTSV4RRFFQ69G5FAV");
        std::fs::create_dir_all(&owned).expect("create owned");
        std::fs::write(
            owned.join(OWNER_MARKER),
            format!("{OWNER_MARKER_VERSION} 01ARZ3NDEKTSV4RRFFQ69G5FAV\n"),
        )
        .expect("write marker");
        // A directory without a matching ownership marker is never touched.
        let unrelated = parent.path().join("trellis-testkit-not-owned");
        std::fs::create_dir_all(&unrelated).expect("create unrelated");
        // A marker that does not match its directory name is never touched.
        let mismatched = parent
            .path()
            .join("trellis-testkit-01ARZ3NDEKTSV4RRFFQ69G5FAW");
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
            .join("trellis-testkit-01ARZ3NDEKTSV4RRFFQ69G5FAX");
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
    fn port_lease_holds_and_releases_its_loopback_listeners() {
        let lease = PortLease::reserve().expect("reserve loopback ports");
        let set = lease.ports().expect("read reserved ports");
        let ports = [set.http, set.nats, set.monitor, set.websocket];
        for port in ports {
            assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        }
        lease.release_for_spawn();
        let _listeners: Vec<_> = ports
            .into_iter()
            .map(|port| {
                TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("released distinct listener")
            })
            .collect();
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
