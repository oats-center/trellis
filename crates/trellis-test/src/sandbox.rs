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
    #[cfg(unix)]
    {
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(TrellisTestError::new(
            TrellisTestErrorKind::UnsupportedPlatform,
            TrellisTestStage::Validation,
            "trellis-test supports Linux and macOS only",
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
/// Only the current server/local-NATS address-in-use diagnostics count; any
/// other failure must not be retried.
pub(crate) fn is_port_conflict(diagnostics: &str, ports: &PortSet) -> bool {
    let lowered = diagnostics.to_ascii_lowercase();
    let address_in_use = lowered.contains("address already in use")
        || lowered.contains("addrinuse")
        || ports
            .all()
            .iter()
            .any(|port| lowered.contains(&format!("port {port} is already in use")));
    address_in_use
        && ports
            .all()
            .iter()
            .any(|port| diagnostics.contains(&port.to_string()))
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
