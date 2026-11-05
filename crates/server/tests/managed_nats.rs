//! A server that runs its own NATS tells clients where to reach it, and
//! listens for them where they connect from.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An installation `init` generated with `init_args`, running on reserved ports
/// `[trellis, nats, monitor, websocket]`.
struct Installation {
    running: tokio::process::Child,
    ports: Vec<u16>,
    config_dir: PathBuf,
    _reserved: Vec<trellis_local_nats::ReservedPort>,
}

impl Installation {
    async fn start(root: &Path, init_args: &[&str]) -> Self {
        Self::start_on(root, init_args, None).await
    }

    /// Like [`Installation::start`], with the Trellis and WebSocket ports a
    /// proxy already forwards to.
    async fn start_on(root: &Path, init_args: &[&str], fronted: Option<(u16, u16)>) -> Self {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let server = env!("CARGO_BIN_EXE_trellis-server");
        let reserved: Vec<_> = (0..4)
            .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
            .collect();
        let mut ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
        if let Some((trellis, websocket)) = fronted {
            (ports[0], ports[3]) = (trellis, websocket);
        }
        let config_dir = root.join("installation");
        let generated = tokio::process::Command::new(server)
            .arg("init")
            .args(init_args)
            .arg(&config_dir)
            .args(["--trellis-port", &ports[0].to_string()])
            .args(["--nats-port", &ports[1].to_string()])
            .args(["--nats-monitor-port", &ports[2].to_string()])
            .args(["--nats-ws-port", &ports[3].to_string()])
            .output()
            .await
            .unwrap();
        assert!(
            generated.status.success(),
            "init failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let running = tokio::process::Command::new(server)
            .arg("--config")
            .arg(&config_dir)
            .arg("--nats-download")
            .arg("--local-nats-cache")
            .arg(workspace.join("target/trellis-test-cache"))
            .arg("--local-nats-ports")
            .arg(format!("{},{},{}", ports[1], ports[2], ports[3]))
            .arg("--isolated-process-group")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            running,
            ports,
            config_dir,
            _reserved: reserved,
        }
    }

    /// Waits until the managed broker accepts WebSocket connections locally.
    async fn wait_for_websocket(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::net::TcpStream::connect(("127.0.0.1", self.ports[3]))
            .await
            .is_err()
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the managed broker never listened for WebSockets"
            );
            assert!(
                self.running.try_wait().unwrap().is_none(),
                "the server exited before its broker listened"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn stop(mut self) {
        let group = self.running.id().unwrap() as i32;
        // SAFETY: signals the process group this test started and owns.
        unsafe { libc::kill(-group, libc::SIGTERM) };
        tokio::time::timeout(Duration::from_secs(30), self.running.wait())
            .await
            .unwrap()
            .unwrap();
    }
}

/// Connects to the WebSocket port at a loopback address other than
/// 127.0.0.1. Linux accepts it only for a listener on every interface, so it
/// stands in for a proxy on another host.
async fn connect_from_elsewhere(port: u16) -> std::io::Result<tokio::net::TcpStream> {
    tokio::net::TcpStream::connect(("127.0.0.2", port)).await
}

/// Clients on other machines must be given the address `init` chose for
/// them, never the loopback address the server itself uses for its broker.
/// The broker's plaintext WebSocket stays private to the Trellis port that
/// relays it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_nats_is_advertised_at_its_configured_address() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let mut installation = Installation::start(directory.path(), &["--dev"]).await;

    let issuer = format!("https://localhost:{}", installation.ports[0]);
    let roots = [installation.config_dir.join("tls/ca.crt")];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let discovered = loop {
        match trellis_rs::auth::discover_native_oauth(&issuer, &roots).await {
            Ok(discovered) => break discovered,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the server never served sign-in: {error}"
                );
                assert!(
                    installation.running.try_wait().unwrap().is_none(),
                    "the server exited before serving sign-in"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    assert_eq!(
        discovered.nats_servers,
        vec![format!("tls://localhost:{}", installation.ports[1])]
    );
    installation.wait_for_websocket().await;
    assert!(
        connect_from_elsewhere(installation.ports[3]).await.is_err(),
        "the relayed plaintext WebSocket accepted a connection from outside loopback"
    );

    installation.stop().await;
}

/// A load balancer on another host terminates HTTPS and forwards WebSocket
/// upgrades straight to the broker's plaintext listener.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_nats_websocket_behind_a_proxy_accepts_the_proxy() {
    let directory = tempfile::tempdir().unwrap();
    let mut installation = Installation::start(
        directory.path(),
        &[
            "--tls",
            "generated",
            "--trusted-proxy",
            "127.0.0.0/8",
            "--nats-websocket-url",
            "wss://trellis.example.test",
            "--nats-websocket-behind-proxy",
            "--nats-websocket-only",
        ],
    )
    .await;
    installation.wait_for_websocket().await;
    // Behind the trusted proxy the Trellis port serves plain HTTP; wait until
    // the runtime is ready so stopping it later is an ordinary shutdown.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let ready = async {
            let mut probe = tokio::net::TcpStream::connect(("127.0.0.1", installation.ports[0]))
                .await
                .ok()?;
            probe
                .write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .ok()?;
            let mut status = [0; 12];
            probe.read_exact(&mut status).await.ok()?;
            (&status == b"HTTP/1.1 200").then_some(())
        };
        if ready.await.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never became ready"
        );
        assert!(
            installation.running.try_wait().unwrap().is_none(),
            "the server exited before becoming ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let mut proxied = connect_from_elsewhere(installation.ports[3])
        .await
        .expect("the proxy could not reach the broker's WebSocket listener");
    proxied
        .write_all(
            b"GET / HTTP/1.1\r\nHost: trellis.example.test\r\nConnection: Upgrade\r\n\
            Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = [0; 12];
    tokio::time::timeout(Duration::from_secs(10), proxied.read_exact(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&response, b"HTTP/1.1 101");

    installation.stop().await;
}

/// Writes a CA and leaf certificates for `[trellis_hosts, nats_hosts]` into
/// `directory`, as an operator's own CA would issue them.
fn issue_certificates(directory: &Path, trellis_hosts: &[&str], nats_hosts: &[&str]) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::default();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    std::fs::write(
        directory.join("ca.crt"),
        ca.self_signed(&ca_key).unwrap().pem(),
    )
    .unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    for (name, hosts) in [("trellis", trellis_hosts), ("nats", nats_hosts)] {
        let key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(
            hosts
                .iter()
                .map(|host| host.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
        std::fs::write(directory.join(format!("{name}.crt")), leaf.pem()).unwrap();
        std::fs::write(directory.join(format!("{name}.key")), key.serialize_pem()).unwrap();
    }
}

/// Sends `request` to `https://localhost:port`, trusting `ca`, and returns
/// what arrives until the server closes or pauses.
async fn tls_exchange(port: u16, ca: PathBuf, request: String) -> Vec<u8> {
    tokio::task::spawn_blocking(move || {
        use rustls::pki_types::pem::PemObject as _;
        use std::io::{Read as _, Write as _};
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from_pem_file(ca).unwrap())
            .unwrap();
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connection =
            rustls::ClientConnection::new(config.into(), "localhost".try_into().unwrap()).unwrap();
        let mut received = Vec::new();
        let Ok(socket) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
            return received;
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        if stream.write_all(request.as_bytes()).is_err() {
            return received;
        }
        let mut chunk = [0; 4096];
        while let Ok(read @ 1..) = stream.read(&mut chunk) {
            received.extend_from_slice(&chunk[..read]);
        }
        received
    })
    .await
    .unwrap()
}

/// An operator's NATS certificate names the host clients use, not loopback,
/// yet the server reaches the broker it runs at loopback; and browsers reach
/// NATS through Trellis's address, never the server's own loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supplied_certificates_serve_nats_by_the_names_clients_use() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let certificates = directory.path().join("certificates");
    std::fs::create_dir(&certificates).unwrap();
    issue_certificates(&certificates, &["localhost"], &["nats.example.test"]);
    let file = |name: &str| certificates.join(name).display().to_string();
    let mut installation = Installation::start(
        directory.path(),
        &[
            "--tls",
            "supplied",
            "--tls-certificate-chain",
            &file("trellis.crt"),
            "--tls-private-key",
            &file("trellis.key"),
            "--nats-tls-certificate-chain",
            &file("nats.crt"),
            "--nats-tls-private-key",
            &file("nats.key"),
            "--tls-ca-certificate",
            &file("ca.crt"),
            "--nats-server-url",
            "tls://nats.example.test:4222",
        ],
    )
    .await;

    let ca = certificates.join("ca.crt");
    let port = installation.ports[0];
    let get = |path: &str| {
        format!("GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while !tls_exchange(port, ca.clone(), get("/readyz"))
        .await
        .starts_with(b"HTTP/1.1 200")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never became ready"
        );
        assert!(
            installation.running.try_wait().unwrap().is_none(),
            "the server exited before becoming ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Browsers are sent to NATS through Trellis's own address, which the
    // operator's Trellis certificate already covers.
    let discovery = String::from_utf8(
        tls_exchange(
            port,
            ca.clone(),
            get("/.well-known/oauth-authorization-server"),
        )
        .await,
    )
    .unwrap();
    let origin = format!("wss://localhost:{port}");
    assert!(
        discovery.contains(&format!("\"{origin}\"")),
        "browsers aren't sent to {origin}: {discovery}"
    );
    let upgrade = tls_exchange(
        port,
        ca,
        format!(
            "GET / HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: Upgrade\r\n\
            Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        ),
    )
    .await;
    assert!(
        upgrade.starts_with(b"HTTP/1.1 101"),
        "the WebSocket upgrade didn't reach NATS: {}",
        String::from_utf8_lossy(&upgrade)
    );

    installation.stop().await;
}

/// An HTTPS-terminating proxy for `localhost`, as a load balancer runs one:
/// it sends every connection that asks for a WebSocket upgrade to
/// `websocket_port` and the rest to `trellis_port`, both in plaintext. Returns
/// its port and how many WebSocket connections it has carried.
async fn start_proxy(
    certificates: &Path,
    trellis_port: u16,
    websocket_port: u16,
) -> (
    u16,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use rustls::pki_types::pem::PemObject as _;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let chain = rustls::pki_types::CertificateDer::pem_file_iter(certificates.join("trellis.crt"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key =
        rustls::pki_types::PrivateKeyDer::from_pem_file(certificates.join("trellis.key")).unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
    ));
    // `localhost` may resolve to either loopback, so the proxy answers on both.
    let ipv4 = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = ipv4.local_addr().unwrap().port();
    let ipv6 = tokio::net::TcpListener::bind(("::1", port)).await.unwrap();
    let upgrades = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let serve = |listener: tokio::net::TcpListener| {
        let acceptor = acceptor.clone();
        let counted = upgrades.clone();
        async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let counted = counted.clone();
                tokio::spawn(async move {
                    let Ok(mut client) = acceptor.accept(socket).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut byte = [0; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if client.read_exact(&mut byte).await.is_err() || head.len() > 65536 {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let websocket = String::from_utf8_lossy(&head)
                        .to_ascii_lowercase()
                        .contains("\r\nupgrade: websocket");
                    if websocket {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    let backend = if websocket {
                        websocket_port
                    } else {
                        trellis_port
                    };
                    let Ok(mut upstream) =
                        tokio::net::TcpStream::connect(("127.0.0.1", backend)).await
                    else {
                        return;
                    };
                    if upstream.write_all(&head).await.is_ok() {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                });
            }
        }
    };
    let (ipv4, ipv6) = (serve(ipv4), serve(ipv6));
    let task = tokio::spawn(async move {
        tokio::join!(ipv4, ipv6);
    });
    (port, upgrades, task)
}

/// Waits until `GET /readyz` through the proxy at `port`, trusting `ca`, answers 200.
async fn wait_ready_through(port: u16, ca: &Path, server: &mut tokio::process::Child) {
    let request =
        format!("GET /readyz HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while !tls_exchange(port, ca.to_path_buf(), request.clone())
        .await
        .starts_with(b"HTTP/1.1 200")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never became ready through the proxy"
        );
        assert!(
            server.try_wait().unwrap().is_none(),
            "the server exited before becoming ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Upgrades a WebSocket through the proxy at `port` and returns the status line.
async fn websocket_through(port: u16, ca: &Path) -> Vec<u8> {
    tls_exchange(
        port,
        ca.to_path_buf(),
        format!(
            "GET / HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: Upgrade\r\n\
            Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        ),
    )
    .await
}

/// With the proxy holding every certificate, Trellis reaches the NATS it runs
/// in plaintext on loopback, sign-in and the WebSocket are served through the
/// proxy, and native NATS can't be reached from anywhere else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_holding_every_certificate_fronts_trellis_and_its_nats() {
    let directory = tempfile::tempdir().unwrap();
    let certificates = directory.path().join("certificates");
    std::fs::create_dir(&certificates).unwrap();
    issue_certificates(&certificates, &["localhost"], &["unused.example.test"]);
    let ca = certificates.join("ca.crt");
    // The proxy's address is the public one, so it is known before `init`; its
    // backends listen once the installation starts.
    let reserved: Vec<_> = (0..2)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect();
    let (trellis_port, websocket_port) = (reserved[0].port(), reserved[1].port());
    let (proxy, _, proxy_task) = start_proxy(&certificates, trellis_port, websocket_port).await;
    drop(reserved);
    let public = format!("https://localhost:{proxy}");
    let websocket = format!("wss://localhost:{proxy}");
    let mut installation = Installation::start_on(
        directory.path(),
        &[
            "--tls",
            "proxy",
            "--trusted-proxy",
            "127.0.0.0/8",
            "--trellis-origin",
            &public,
            "--nats-websocket-url",
            &websocket,
            "--nats-websocket-behind-proxy",
            "--nats-websocket-only",
        ],
        Some((trellis_port, websocket_port)),
    )
    .await;
    wait_ready_through(proxy, &ca, &mut installation.running).await;

    let discovery = String::from_utf8(
        tls_exchange(
            proxy,
            ca.clone(),
            format!(
                "GET /.well-known/oauth-authorization-server HTTP/1.1\r\n\
                Host: localhost:{proxy}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await,
    )
    .unwrap();
    assert!(
        discovery.contains(&format!("\"{websocket}\"")),
        "clients aren't sent to {websocket}: {discovery}"
    );
    let upgrade = websocket_through(proxy, &ca).await;
    assert!(
        upgrade.starts_with(b"HTTP/1.1 101"),
        "the WebSocket upgrade didn't reach NATS: {}",
        String::from_utf8_lossy(&upgrade)
    );
    assert!(
        connect_from_elsewhere(installation.ports[1]).await.is_err(),
        "plaintext native NATS accepted a connection from outside loopback"
    );

    installation.stop().await;
    proxy_task.abort();
}

/// Trellis can reach a NATS server it doesn't run through the proxy's
/// WebSocket, like every other client, trusting the proxy's certificate the
/// way the system does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trellis_reaches_external_nats_through_the_proxy() {
    let directory = tempfile::tempdir().unwrap();
    let certificates = directory.path().join("certificates");
    std::fs::create_dir(&certificates).unwrap();
    issue_certificates(&certificates, &["localhost"], &["unused.example.test"]);
    let ca = certificates.join("ca.crt");
    let reserved: Vec<_> = (0..4)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect();
    let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
    let (proxy, upgrades, proxy_task) = start_proxy(&certificates, ports[0], ports[3]).await;
    let public = format!("https://localhost:{proxy}");
    let websocket = format!("wss://localhost:{proxy}");
    let installation = directory.path().join("installation");
    let data = directory.path().join("nats-data");
    let server = env!("CARGO_BIN_EXE_trellis-server");
    let generated = tokio::process::Command::new(server)
        .arg("init")
        .arg(&installation)
        .args(["--tls", "proxy", "--trusted-proxy", "127.0.0.0/8"])
        .args(["--external-nats", "--nats-data-dir"])
        .arg(&data)
        .args(["--trellis-origin", &public])
        .args(["--nats-server-url", &websocket])
        .args(["--nats-websocket-url", &websocket])
        .args(["--nats-websocket-behind-proxy", "--nats-websocket-only"])
        .args(["--trellis-port", &ports[0].to_string()])
        .args(["--nats-port", &ports[1].to_string()])
        .args(["--nats-monitor-port", &ports[2].to_string()])
        .args(["--nats-ws-port", &ports[3].to_string()])
        .output()
        .await
        .unwrap();
    assert!(
        generated.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    drop(reserved);
    let nats_bin = std::env::var_os("TRELLIS_TEST_NATS_BIN")
        .expect("TRELLIS_TEST_NATS_BIN must point to the pinned nats-server");
    let mut nats = tokio::process::Command::new(nats_bin)
        .arg("-c")
        .arg(installation.join("nats.conf"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(("127.0.0.1", ports[3]))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "NATS never listened for WebSockets"
        );
        assert!(nats.try_wait().unwrap().is_none(), "NATS exited");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut running = tokio::process::Command::new(server)
        .arg("--config")
        .arg(&installation)
        .arg("--isolated-process-group")
        // The proxy's CA stands in for the public roots a real proxy's
        // certificate chains to.
        .env("SSL_CERT_FILE", &ca)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_ready_through(proxy, &ca, &mut running).await;
    assert!(
        upgrades.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "Trellis became ready without reaching NATS through the proxy"
    );

    let group = running.id().unwrap() as i32;
    // SAFETY: signals the process group this test started and owns.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    tokio::time::timeout(Duration::from_secs(30), running.wait())
        .await
        .unwrap()
        .unwrap();
    nats.kill().await.unwrap();
    proxy_task.abort();
}
