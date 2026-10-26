//! Private sandbox directories, child environments, and automatic port leases.
//!
//! Each startup reserves five loopback listeners outside Linux's automatic
//! outbound port range, holds them until immediately before spawn, and closes
//! them in one narrow step. Same-port UDP ownership survives that handoff so
//! other Rust and Node harnesses cannot steal released TCP ports during startup.
//! No lock files, PID-derived ports, or global mutex are used.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
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

/// The five loopback ports reserved for one startup attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PortSet {
    /// Trellis HTTP listener.
    pub http: u16,
    /// Isolated OAuth listener.
    pub oauth: u16,
    /// Managed NATS native listener.
    pub nats: u16,
    /// Managed NATS monitoring listener.
    pub monitor: u16,
    /// Managed NATS WebSocket listener.
    pub websocket: u16,
}

/// Five TCP reservations with same-port UDP handoff ownership.
///
/// Each TCP side is a bound, never-listening `SO_REUSEADDR` socket. A listening
/// reservation would be inherited by a process another thread spawns, until it
/// execs, and could keep accepting after the lease closed it.
pub(crate) struct PortLease {
    http: (socket2::Socket, UdpSocket),
    oauth: (socket2::Socket, UdpSocket),
    nats: (socket2::Socket, UdpSocket),
    monitor: (socket2::Socket, UdpSocket),
    websocket: (socket2::Socket, UdpSocket),
}

impl PortLease {
    /// Reserves distinct loopback ports, holding each until spawn.
    pub(crate) fn reserve() -> Result<Self, TrellisTestError> {
        // HTTP/WSS clients reject Fetch's restricted low ports (through 10080).
        const MIN_AUTOMATIC_PORT: u32 = 10_240;
        let failure = |message: String| {
            TrellisTestError::new(
                TrellisTestErrorKind::PortAllocation,
                TrellisTestStage::PortAllocation,
                message,
            )
        };
        let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
            .map_err(|error| failure(format!("reading the automatic TCP port range: {error}")))?;
        let bounds = range
            .split_whitespace()
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| failure(format!("parsing the automatic TCP port range: {error}")))?;
        let [first, last] = bounds.as_slice() else {
            return Err(failure("invalid automatic TCP port range".into()));
        };
        if first > last {
            return Err(failure("invalid automatic TCP port range".into()));
        }
        let lower_count = u32::from(*first).saturating_sub(MIN_AUTOMATIC_PORT);
        let upper_start = (u32::from(*last) + 1).max(MIN_AUTOMATIC_PORT);
        let available_count = lower_count + 65536 - upper_start;
        if available_count == 0 {
            return Err(failure(
                "no nonprivileged listener ports outside the kernel ephemeral range".into(),
            ));
        }
        let bind = |role: &str| -> Result<(socket2::Socket, UdpSocket), TrellisTestError> {
            for _ in 0..32 {
                let mut bytes = [0; 4];
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut source| source.read_exact(&mut bytes))
                    .map_err(|error| {
                        failure(format!("selecting the {role} loopback port: {error}"))
                    })?;
                let offset = u32::from_ne_bytes(bytes) % available_count;
                let selected = if offset < lower_count {
                    MIN_AUTOMATIC_PORT + offset
                } else {
                    upper_start + offset - lower_count
                };
                let selected = u16::try_from(selected)
                    .expect("selected port is within the validated nonprivileged range");
                let ownership = match UdpSocket::bind((Ipv4Addr::LOCALHOST, selected)) {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                    Err(error) => {
                        return Err(failure(format!(
                            "reserving the {role} handoff ownership: {error}"
                        )))
                    }
                };
                let socket =
                    socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
                        .and_then(|socket| {
                            socket.set_reuse_address(true)?;
                            Ok(socket)
                        })
                        .map_err(|error| {
                            failure(format!("creating the {role} reservation: {error}"))
                        })?;
                // Fails while anything listens on the port, even with SO_REUSEADDR.
                match socket.bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, selected).into()) {
                    Ok(()) => return Ok((socket, ownership)),
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                    Err(error) => {
                        return Err(failure(format!(
                            "reserving the {role} loopback port: {error}"
                        )))
                    }
                }
            }
            Err(failure(format!(
                "no available non-ephemeral {role} loopback port"
            )))
        };
        Ok(Self {
            http: bind("HTTP")?,
            oauth: bind("OAuth")?,
            nats: bind("NATS")?,
            monitor: bind("monitor")?,
            websocket: bind("WebSocket")?,
        })
    }

    /// The selected ports.
    pub(crate) fn ports(&self) -> Result<PortSet, TrellisTestError> {
        let port_of = |socket: &socket2::Socket, role: &str| -> Result<u16, TrellisTestError> {
            socket
                .local_addr()
                .and_then(|addr| {
                    addr.as_socket()
                        .map(|addr| addr.port())
                        .ok_or_else(|| std::io::Error::other("not an IP socket"))
                })
                .map_err(|error| {
                    TrellisTestError::new(
                        TrellisTestErrorKind::PortAllocation,
                        TrellisTestStage::PortAllocation,
                        format!("reading the reserved {role} port: {error}"),
                    )
                })
        };
        Ok(PortSet {
            http: port_of(&self.http.0, "HTTP")?,
            oauth: port_of(&self.oauth.0, "OAuth")?,
            nats: port_of(&self.nats.0, "NATS")?,
            monitor: port_of(&self.monitor.0, "monitor")?,
            websocket: port_of(&self.websocket.0, "WebSocket")?,
        })
    }

    /// Closes the TCP reservations, retaining exclusive ownership through startup.
    pub(crate) fn release_for_spawn(self) -> [UdpSocket; 5] {
        drop(self.http.0);
        drop(self.oauth.0);
        drop(self.nats.0);
        drop(self.monitor.0);
        drop(self.websocket.0);
        [
            self.http.1,
            self.oauth.1,
            self.nats.1,
            self.monitor.1,
            self.websocket.1,
        ]
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
/// remove; [`crate::TrellisTestRuntime::shutdown`] is the per-runtime guarantee.
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
    use std::net::TcpListener;

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
    fn released_ports_survive_automatic_outbound_allocation() {
        // A sibling test's pre-exec child can inherit our listeners before
        // CLOEXEC closes them. Probe allocation in its own process so release
        // is not confused with an unrelated fork retaining the same socket.
        const NAME: &str = "sandbox::tests::released_ports_survive_automatic_outbound_allocation";
        if !std::env::args().any(|argument| argument == NAME) {
            let status =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", NAME, "--nocapture"])
                    .status()
                    .expect("run isolated allocator probe");
            assert!(
                status.success(),
                "isolated allocator probe failed: {status}"
            );
            return;
        }
        // Exercise the host's real automatic allocation without requiring
        // privileged namespaces or changing its network configuration.
        let peer = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("outbound destination");
        let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
            .expect("kernel automatic port range");
        let bounds: Vec<u16> = range
            .split_whitespace()
            .map(|value| value.parse().expect("kernel port"))
            .collect();
        let automatic = bounds[0]..=bounds[1];
        let lease = PortLease::reserve().expect("reserve listeners");
        let set = lease.ports().expect("reserved ports");
        for port in [set.http, set.oauth, set.nats, set.monitor, set.websocket] {
            // A readiness probe must never mistake the reservation for a server.
            assert!(std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
            assert!(
                !automatic.contains(&port),
                "listener exposed to outbound reuse"
            );
        }
        let _ownership = lease.release_for_spawn();
        let _connections: Vec<_> = (0..8)
            .map(|_| {
                std::net::TcpStream::connect_timeout(
                    &peer.local_addr().expect("outbound address"),
                    std::time::Duration::from_secs(2),
                )
                .inspect(|connection| {
                    assert!(automatic.contains(&connection.local_addr().unwrap().port()));
                })
                .expect("occupy an automatic outbound port")
            })
            .collect();
        let _listeners: Vec<_> = [set.http, set.oauth, set.nats, set.monitor, set.websocket]
            .into_iter()
            .map(|port| {
                TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                    .expect("released listener survives outbound allocation")
            })
            .collect();
    }

    #[test]
    fn node_reservations_respect_rust_handoff_ownership() {
        let lease = PortLease::reserve().expect("reserve listeners");
        let port = lease.ports().expect("reserved ports").http;
        let mut ownership = Some(lease.release_for_spawn());
        let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ts/packages/trellis-testkit/src/ports.ts")
            .canonicalize()
            .expect("Node allocator source");
        let script = format!(
            r#"
            import assert from "node:assert/strict";
            import {{ reserveLocalPort }} from {};
            if (process.argv[1] === "held") {{
                await assert.rejects(() => reserveLocalPort({port}), {{ code: "EADDRINUSE" }});
            }} else {{
                const lease = await reserveLocalPort({port});
                await lease.release();
            }}
            "#,
            serde_json::to_string(&module).expect("module path")
        );
        for phase in ["held", "released"] {
            if phase == "released" {
                drop(ownership.take());
            }
            let output = std::process::Command::new("node")
                .args([
                    "--experimental-transform-types",
                    "--input-type=module",
                    "--eval",
                    &script,
                    phase,
                ])
                .output()
                .expect("run the ordinary Node allocator");
            assert!(
                output.status.success(),
                "{phase}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
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
