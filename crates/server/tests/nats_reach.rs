//! Trellis reaches the NATS servers clients use, and a development
//! installation keeps the NATS server it runs to itself.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SERVER: &str = env!("CARGO_BIN_EXE_trellis-server");

fn reserve(count: usize) -> Vec<trellis_local_nats::ReservedPort> {
    (0..count)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect()
}

fn nats_cache() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/trellis-test-cache")
}

/// Signals the process group `child` leads and waits for it to exit.
async fn stop(mut child: tokio::process::Child) {
    let group = child.id().unwrap() as i32;
    // SAFETY: signals the process group this test started and owns.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .unwrap()
        .unwrap();
}

/// A one-server NATS cluster from `trellis-server nats new`, run the way an
/// operator runs it, on `[client, websocket, monitor]`.
struct OperatorNats {
    running: tokio::process::Child,
    folder: PathBuf,
    ports: [u16; 3],
}

impl OperatorNats {
    async fn start(folder: PathBuf) -> Self {
        let reserved = reserve(4);
        let ports = [reserved[0].port(), reserved[1].port(), reserved[2].port()];
        let made = tokio::process::Command::new(SERVER)
            .args(["nats", "new", "--servers", "1", "--out"])
            .arg(&folder)
            .args(["--client-port", &ports[0].to_string()])
            .args(["--websocket-port", &ports[1].to_string()])
            .args(["--monitor-port", &ports[2].to_string()])
            .args(["--cluster-port", &reserved[3].port().to_string()])
            .output()
            .await
            .unwrap();
        assert!(
            made.status.success(),
            "nats new failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        drop(reserved);
        let nats_bin = std::env::var_os("TRELLIS_TEST_NATS_BIN")
            .expect("TRELLIS_TEST_NATS_BIN must point to the pinned nats-server");
        let mut running = tokio::process::Command::new(nats_bin)
            .arg("-c")
            .arg(folder.join("nats-1.conf"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::net::TcpStream::connect(("127.0.0.1", ports[1]))
            .await
            .is_err()
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "NATS never listened for WebSockets"
            );
            assert!(running.try_wait().unwrap().is_none(), "NATS exited");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Self {
            running,
            folder,
            ports,
        }
    }

    /// The `[nats]` section that hands Trellis this operator's files.
    fn section(&self, servers: &str) -> String {
        format!(
            "[nats]\nservers = \"{servers}\"\nsystem_credentials = {}\noperator_signing_key = {}\n",
            quoted(&self.folder.join("system.creds")),
            quoted(&self.folder.join("trellis-operator.nk")),
        )
    }
}

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

/// Starts `trellis-server --config <folder>` with `args`.
fn start_trellis(folder: &Path, args: &[&str], ca: Option<&Path>) -> tokio::process::Child {
    let mut command = tokio::process::Command::new(SERVER);
    command
        .arg("--config")
        .arg(folder)
        .args(args)
        .arg("--isolated-process-group")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(ca) = ca {
        // The test CA stands in for the public roots a real certificate
        // chains to.
        command.env("SSL_CERT_FILE", ca);
    }
    command.spawn().unwrap()
}

/// A development installation advertises its NATS at loopback, and the
/// plaintext WebSocket the Trellis port relays stays private to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_development_installation_keeps_its_nats_to_itself() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().join("dev");
    let reserved = reserve(4);
    let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
    let nats_ports = format!("{},{},{}", ports[1], ports[2], ports[3]);
    let cache = nats_cache();
    let managed = [
        "--dev",
        "--nats-download",
        "--local-nats-cache",
        cache.to_str().unwrap(),
        "--local-nats-ports",
        &nats_ports,
    ];
    // The first start creates the installation; a check stops there, and
    // reports the database it hasn't created yet.
    let created = tokio::process::Command::new(SERVER)
        .arg("check")
        .arg("--config")
        .arg(&folder)
        .args(managed)
        .output()
        .await
        .unwrap();
    let config = folder.join("config.toml");
    assert!(
        config.is_file(),
        "creating the installation failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let edited = std::fs::read_to_string(&config)
        .unwrap()
        .replace(":3000", &format!(":{}", ports[0]))
        .replace("port = 3000", &format!("port = {}", ports[0]));
    std::fs::write(&config, edited).unwrap();
    drop(reserved);
    let mut running = start_trellis(&folder, &managed, None);

    let issuer = format!("http://localhost:{}", ports[0]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let discovered = loop {
        match trellis_rs::auth::discover_native_oauth(&issuer, &[]).await {
            Ok(discovered) => break discovered,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the server never served sign-in: {error}"
                );
                assert!(
                    running.try_wait().unwrap().is_none(),
                    "the server exited before serving sign-in"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    assert_eq!(
        discovered.nats_servers,
        vec![format!("nats://127.0.0.1:{}", ports[1])]
    );
    assert!(
        connect_from_elsewhere(ports[3]).await.is_err(),
        "the relayed plaintext WebSocket accepted a connection from outside loopback"
    );
    assert!(
        connect_from_elsewhere(ports[1]).await.is_err(),
        "plaintext native NATS accepted a connection from outside loopback"
    );

    stop(running).await;
}

/// Connects to the WebSocket port at a loopback address other than
/// 127.0.0.1. Linux accepts it only for a listener on every interface, so it
/// stands in for a proxy on another host.
async fn connect_from_elsewhere(port: u16) -> std::io::Result<tokio::net::TcpStream> {
    tokio::net::TcpStream::connect(("127.0.0.2", port)).await
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

/// Trellis creates its accounts on NATS servers it doesn't run, sends
/// browsers to its own address for NATS, and relays their WebSocket upgrades
/// to those servers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trellis_relays_websockets_to_nats_it_does_not_run() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let certificates = directory.path().join("certificates");
    std::fs::create_dir(&certificates).unwrap();
    issue_certificates(&certificates, &["localhost"], &["unused.example.test"]);
    let nats = OperatorNats::start(directory.path().join("nats")).await;
    let reserved = reserve(1);
    let port = reserved[0].port();
    let folder = directory.path().join("trellis");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(
        folder.join("config.toml"),
        format!(
            "[http]\npublic_origin = \"https://localhost:{port}\"\nport = {port}\n\
            nats_websocket_upstream = \"127.0.0.1:{websocket}\"\n\n\
            [http.transport]\nmode = \"tls\"\ncertificate_chain = {chain}\nprivate_key = {key}\n\n{nats}",
            websocket = nats.ports[1],
            chain = quoted(&certificates.join("trellis.crt")),
            key = quoted(&certificates.join("trellis.key")),
            nats = nats.section(&format!("nats://127.0.0.1:{}", nats.ports[0])),
        ),
    )
    .unwrap();
    drop(reserved);
    let mut running = start_trellis(&folder, &[], None);

    let ca = certificates.join("ca.crt");
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
            running.try_wait().unwrap().is_none(),
            "the server exited before becoming ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

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
    let upgrade = websocket_through(port, &ca).await;
    assert!(
        upgrade.starts_with(b"HTTP/1.1 101"),
        "the WebSocket upgrade didn't reach NATS: {}",
        String::from_utf8_lossy(&upgrade)
    );

    stop(running).await;
    drop(nats.running);
}

/// Behind an HTTPS proxy, Trellis reaches NATS through the proxy's
/// WebSocket like every other client, trusting the proxy's certificate the
/// way the system does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trellis_reaches_nats_through_the_proxy() {
    let directory = tempfile::tempdir().unwrap();
    let certificates = directory.path().join("certificates");
    std::fs::create_dir(&certificates).unwrap();
    issue_certificates(&certificates, &["localhost"], &["unused.example.test"]);
    let ca = certificates.join("ca.crt");
    let nats = OperatorNats::start(directory.path().join("nats")).await;
    let reserved = reserve(1);
    let port = reserved[0].port();
    let (proxy, upgrades, proxy_task) = start_proxy(&certificates, port, nats.ports[1]).await;
    let websocket = format!("wss://localhost:{proxy}");
    let folder = directory.path().join("trellis");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(
        folder.join("config.toml"),
        format!(
            "[http]\npublic_origin = \"https://localhost:{proxy}\"\nport = {port}\n\n\
            [http.transport]\nmode = \"proxy\"\ntrusted_proxies = [\"127.0.0.0/8\", \"::1/128\"]\n\n{}\n\
            [client]\nnats_servers = []\nws_nats_servers = [\"{websocket}\"]\n",
            nats.section(&websocket),
        ),
    )
    .unwrap();
    drop(reserved);
    let mut running = start_trellis(&folder, &[], Some(&ca));
    wait_ready_through(proxy, &ca, &mut running).await;
    assert!(
        upgrades.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "Trellis became ready without reaching NATS through the proxy"
    );
    let upgrade = websocket_through(proxy, &ca).await;
    assert!(
        upgrade.starts_with(b"HTTP/1.1 101"),
        "a client's WebSocket upgrade didn't reach NATS: {}",
        String::from_utf8_lossy(&upgrade)
    );

    stop(running).await;
    drop(nats.running);
    proxy_task.abort();
}
