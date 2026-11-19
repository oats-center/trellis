//! Where a running service is in its life, and who is told.
//!
//! A service is *serving* once it is connected, its build is installed, and
//! every registered handler, job worker and consumer takes work. The phase is
//! reported in each health heartbeat, to systemd through `NOTIFY_SOCKET` when
//! it is set, and to an optional readiness file or HTTP probe.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::watch;

/// How long a stopping service waits for the work it holds, by default.
///
/// It fits inside Kubernetes' default 30-second termination grace period.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(25);

/// Where a service is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePhase {
    /// Connected and starting its handlers, or reconnecting after losing its
    /// connection. It takes no work.
    Connecting,
    /// Every registered handler, job worker and consumer takes work.
    Serving,
    /// It takes no new work and is finishing what it holds.
    Stopping,
    /// It has stopped and closed its connection.
    Stopped,
}

impl ServicePhase {
    /// The phase's name, as heartbeats and the readiness probe report it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Serving => "serving",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

impl std::fmt::Display for ServicePhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Readiness and stop options a service connects with.
#[derive(Clone, Debug)]
pub(crate) struct LifecycleOptions {
    pub(crate) readiness_file: Option<PathBuf>,
    pub(crate) readiness_http: Option<SocketAddr>,
    pub(crate) stop_timeout: Duration,
}

impl Default for LifecycleOptions {
    fn default() -> Self {
        Self {
            readiness_file: None,
            readiness_http: None,
            stop_timeout: DEFAULT_STOP_TIMEOUT,
        }
    }
}

/// A connected service's lifecycle: its phase, and a way to stop it.
///
/// Obtain it with [`ConnectedServiceRuntime::lifecycle`] before calling
/// [`ConnectedServiceRuntime::run`]. Clones share one lifecycle.
///
/// [`ConnectedServiceRuntime::lifecycle`]: crate::service::ConnectedServiceRuntime::lifecycle
/// [`ConnectedServiceRuntime::run`]: crate::service::ConnectedServiceRuntime::run
#[derive(Clone)]
pub struct ServiceLifecycle {
    inner: Arc<Inner>,
}

struct Inner {
    phase: watch::Sender<ServicePhase>,
    /// Whether the service has served at least once.
    served: watch::Sender<bool>,
    stop: watch::Sender<bool>,
    health: std::sync::Mutex<Option<crate::client::ServiceHealth>>,
    options: LifecycleOptions,
    /// Whether systemd hears about this service; a host-embedded one is silent.
    owns_process: bool,
}

impl std::fmt::Debug for ServiceLifecycle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceLifecycle")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

impl ServiceLifecycle {
    /// Start a lifecycle in [`ServicePhase::Connecting`], opening its HTTP
    /// readiness probe when one is configured.
    pub(crate) async fn start(
        options: LifecycleOptions,
        owns_process: bool,
    ) -> std::io::Result<Self> {
        let lifecycle = Self {
            inner: Arc::new(Inner {
                phase: watch::channel(ServicePhase::Connecting).0,
                served: watch::channel(false).0,
                stop: watch::channel(false).0,
                health: std::sync::Mutex::new(None),
                options,
                owns_process,
            }),
        };
        lifecycle.clear_readiness_file();
        if let Some(address) = lifecycle.inner.options.readiness_http {
            let listener = tokio::net::TcpListener::bind(address).await?;
            tokio::spawn(serve_probe(listener, lifecycle.inner.phase.subscribe()));
        }
        lifecycle.status("Connecting to Trellis");
        Ok(lifecycle)
    }

    /// Tell systemd what the service is doing, as `STATUS=`.
    pub(crate) fn status(&self, status: &str) {
        self.notify(&format!("STATUS={status}"));
    }

    fn notify(&self, message: &str) {
        if self.inner.owns_process {
            notify(message);
        }
    }

    /// The current phase.
    #[must_use]
    pub fn phase(&self) -> ServicePhase {
        *self.inner.phase.borrow()
    }

    /// Wait until the service serves for the first time.
    ///
    /// Returns `true` once it has served, at once if it already has, and
    /// `false` if it stops without ever serving.
    pub async fn ready(&self) -> bool {
        let mut served = self.inner.served.subscribe();
        let mut phase = self.inner.phase.subscribe();
        loop {
            if *served.borrow_and_update() {
                return true;
            }
            if *phase.borrow_and_update() == ServicePhase::Stopped {
                return false;
            }
            tokio::select! {
                _ = served.changed() => {}
                _ = phase.changed() => {}
            }
        }
    }

    /// Wait for the next phase change and return the new phase.
    pub async fn changed(&self) -> ServicePhase {
        let mut phase = self.inner.phase.subscribe();
        let _ = phase.changed().await;
        let current = *phase.borrow();
        current
    }

    /// Ask the running service to stop, as SIGTERM does: it stops taking work,
    /// finishes what it holds within its stop timeout, and closes.
    pub fn stop(&self) {
        self.inner.stop.send_replace(true);
    }

    pub(crate) async fn stop_requested(&self) {
        let mut stop = self.inner.stop.subscribe();
        let _ = stop.wait_for(|stop| *stop).await;
    }

    pub(crate) fn stop_timeout(&self) -> Duration {
        self.inner.options.stop_timeout
    }

    /// Report the phase in heartbeats from now on.
    pub(crate) fn attach_health(&self, health: crate::client::ServiceHealth) {
        health.set_phase(heartbeat_phase(self.phase()));
        *self.inner.health.lock().expect("lifecycle health") = Some(health);
    }

    /// Move to `phase`, telling systemd, the probe and the next heartbeat.
    pub(crate) fn set(&self, phase: ServicePhase) {
        let previous = self.inner.phase.send_replace(phase);
        if previous == phase || previous == ServicePhase::Stopped {
            if previous == ServicePhase::Stopped {
                self.inner.phase.send_replace(ServicePhase::Stopped);
            }
            return;
        }
        match phase {
            ServicePhase::Serving => {
                let first = !self.inner.served.send_replace(true);
                self.notify(if first {
                    "READY=1\nSTATUS=Serving"
                } else {
                    "STATUS=Serving"
                });
                if let Some(path) = &self.inner.options.readiness_file {
                    if let Err(error) = std::fs::write(path, "serving\n") {
                        tracing::warn!(
                            %error,
                            path = %path.display(),
                            "readiness file not written"
                        );
                    }
                }
            }
            ServicePhase::Connecting => {
                self.notify("STATUS=Reconnecting to Trellis");
                self.clear_readiness_file();
            }
            ServicePhase::Stopping => {
                self.notify("STOPPING=1\nSTATUS=Stopping");
                self.clear_readiness_file();
            }
            ServicePhase::Stopped => self.clear_readiness_file(),
        }
        if phase != ServicePhase::Stopped {
            if let Some(health) = self.inner.health.lock().expect("lifecycle health").as_ref() {
                health.set_phase(heartbeat_phase(phase));
            }
        }
    }

    fn clear_readiness_file(&self) {
        if let Some(path) = &self.inner.options.readiness_file {
            match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    tracing::warn!(%error, path = %path.display(), "readiness file not removed");
                }
                _ => {}
            }
        }
    }
}

fn heartbeat_phase(phase: ServicePhase) -> crate::__types::trellis::HealthHeartbeatSamplePhase {
    use crate::__types::trellis::HealthHeartbeatSamplePhase as Phase;
    match phase {
        ServicePhase::Connecting => Phase::Connecting,
        ServicePhase::Serving => Phase::Serving,
        ServicePhase::Stopping | ServicePhase::Stopped => Phase::Stopping,
    }
}

/// Answer `GET /ready` and `GET /live` until every lifecycle handle is gone.
async fn serve_probe(listener: tokio::net::TcpListener, phase: watch::Receiver<ServicePhase>) {
    let mut ended = phase.clone();
    let closed = async move { while ended.changed().await.is_ok() {} };
    tokio::pin!(closed);
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            () = &mut closed => return,
        };
        let Ok((mut stream, _)) = accepted else {
            continue;
        };
        let phase = phase.clone();
        tokio::spawn(async move {
            let mut request = Vec::with_capacity(256);
            let mut buffer = [0_u8; 256];
            // The request line is all the probe reads; a slow or oversized
            // client is dropped rather than held.
            let read = tokio::time::timeout(Duration::from_secs(5), async {
                while !request.windows(2).any(|window| window == b"\r\n") && request.len() < 4096 {
                    let count = stream.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                Ok::<_, std::io::Error>(())
            })
            .await;
            if !matches!(read, Ok(Ok(()))) {
                return;
            }
            let line = String::from_utf8_lossy(&request);
            let path = line.split_whitespace().nth(1).unwrap_or_default();
            let current = *phase.borrow();
            let (status, body) = match path {
                "/ready" if current == ServicePhase::Serving => ("200 OK", "ready"),
                "/ready" => ("503 Service Unavailable", current.as_str()),
                "/live" if current != ServicePhase::Stopped => ("200 OK", "live"),
                "/live" => ("503 Service Unavailable", current.as_str()),
                _ => ("404 Not Found", "not found"),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\
                connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

/// Send one `sd_notify` message when `NOTIFY_SOCKET` names a socket.
///
/// A path names a socket file; a leading `@` names one in Linux's abstract
/// namespace. A failure is logged once and never stops the service.
fn notify(message: &str) {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    if let Err(error) = send_notification(&socket, message) {
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(%error, "systemd notification not sent");
        }
    }
}

#[cfg(unix)]
fn send_notification(socket: &std::ffi::OsStr, message: &str) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let datagram = std::os::unix::net::UnixDatagram::unbound()?;
    let bytes = socket.as_bytes();
    if let Some(name) = bytes.strip_prefix(b"@") {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            #[cfg(target_os = "android")]
            use std::os::android::net::SocketAddrExt as _;
            #[cfg(target_os = "linux")]
            use std::os::linux::net::SocketAddrExt as _;
            let address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
            datagram.send_to_addr(message.as_bytes(), &address)?;
            return Ok(());
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let _ = name;
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "abstract notification sockets need Linux",
            ));
        }
    }
    datagram.send_to(message.as_bytes(), std::path::Path::new(socket))?;
    Ok(())
}

#[cfg(not(unix))]
fn send_notification(_socket: &std::ffi::OsStr, _message: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "systemd notification needs a Unix datagram socket",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_probe_reports_ready_only_while_serving_and_live_until_stopped() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let (phase, receiver) = watch::channel(ServicePhase::Connecting);
        tokio::spawn(serve_probe(listener, receiver));
        let get = |path: &'static str| async move {
            let mut stream = tokio::net::TcpStream::connect(address)
                .await
                .expect("connect");
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nhost: probe\r\n\r\n").as_bytes())
                .await
                .expect("write");
            let mut response = String::new();
            stream.read_to_string(&mut response).await.expect("read");
            response
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(get("/ready").await, "503");
        assert_eq!(get("/live").await, "200");
        phase.send_replace(ServicePhase::Serving);
        assert_eq!(get("/ready").await, "200");
        phase.send_replace(ServicePhase::Stopped);
        assert_eq!(get("/ready").await, "503");
        assert_eq!(get("/live").await, "503");
        assert_eq!(get("/other").await, "404");
    }
}
