use super::super::oauth::{OAuthEngine, OAuthListener, OAuthStorage};
use super::*;
use std::sync::Arc;

pub(super) struct Certificate {
    pub(super) directory: tempfile::TempDir,
    pub(super) certificate: std::path::PathBuf,
    pub(super) private_key: std::path::PathBuf,
    pub(super) pem: String,
}

pub(super) fn certificate() -> Certificate {
    let directory = tempfile::TempDir::new().unwrap();
    let certificate = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    let root_certificate = directory.path().join("root.pem");
    let root_key = directory.path().join("root-key.pem");
    let request = directory.path().join("server.csr");
    let extensions = directory.path().join("extensions.cnf");
    std::fs::write(
        &extensions,
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,\
        digitalSignature\nextendedKeyUsage=serverAuth\n\
        subjectAltName=IP:127.0.0.1,IP:127.0.0.2\n",
    )
    .unwrap();
    let mut root = std::process::Command::new("openssl");
    root.args([
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-days",
        "1",
        "-subj",
        "/CN=OAuth fixture CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
        "-keyout",
    ])
    .arg(&root_key)
    .arg("-out")
    .arg(&root_certificate);
    let mut leaf = std::process::Command::new("openssl");
    leaf.args([
        "req",
        "-new",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-subj",
        "/CN=OAuth integration fixture",
        "-keyout",
    ])
    .arg(&private_key)
    .arg("-out")
    .arg(&request);
    let mut sign = std::process::Command::new("openssl");
    sign.args(["x509", "-req", "-in"])
        .arg(&request)
        .arg("-CA")
        .arg(&root_certificate)
        .arg("-CAkey")
        .arg(&root_key)
        .args(["-CAcreateserial", "-days", "1", "-extfile"])
        .arg(&extensions)
        .arg("-out")
        .arg(&certificate);
    for mut command in [root, leaf, sign] {
        assert!(
            command.output().unwrap().status.success(),
            "could not generate the TLS fixture certificate chain"
        );
    }
    let pem = std::fs::read_to_string(&root_certificate).unwrap();
    Certificate {
        directory,
        certificate,
        private_key,
        pem,
    }
}

#[tokio::test]
async fn oauth_tls_serves_http2_and_rejects_untrusted_tls_plaintext_and_foreign_authorities() {
    let certificate = certificate();
    let mut f = fixture();
    let socket = tokio::net::TcpListener::bind("127.0.0.2:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let origin = format!("https://{address}");
    let config_path = certificate.directory.path().join("runtime.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[auth]
instance_id = "{}"
oauth_storage_key_file = "oauth-storage.key"
[auth.oauth]
origin = {origin:?}
bind_address = "127.0.0.1"
port = {}
[auth.oauth.transport]
mode = "tls"
certificate_chain = "certificate.pem"
private_key = "private-key.pem"
[http]
public_origin = "https://127.0.0.1:3000"
"#,
            ulid::Ulid::new(),
            address.port()
        ),
    )
    .unwrap();
    let config = crate::config::RuntimeConfig::load_from_path(&config_path).unwrap();
    let oauth = config.resolve_oauth_listener().unwrap();
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_issuer = origin.clone();
    let admin = f.admin.clone();
    let engine =
        Arc::new(OAuthEngine::new(OAuthStorage::new(f.store, &[17; 32]).unwrap()).unwrap());
    let listener = OAuthListener::load(socket, &oauth.origin, &oauth.transport)
        .await
        .unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let stopping = shutdown.clone();
    let router = super::super::oauth::router(engine.clone(), vec!["tls://127.0.0.1:4222".into()]);
    let server = tokio::spawn(listener.run(router, stopping));
    let metadata = format!("{origin}/.well-known/oauth-authorization-server");
    let untrusted = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(
        untrusted.get(&metadata).send().await.is_err(),
        "the self-signed fixture must not be trusted by default"
    );
    assert!(
        untrusted
            .get(format!(
                "http://{address}/.well-known/oauth-authorization-server"
            ))
            .send()
            .await
            .is_err(),
        "the TLS listener must not downgrade to plaintext"
    );
    let http = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap())
        .http2_prior_knowledge()
        .build()
        .unwrap();
    let response = http.get(&metadata).send().await.unwrap();
    assert_eq!(response.version(), reqwest::Version::HTTP_2);
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let document: serde_json::Value = response.json().await.unwrap();
    assert_eq!(document["issuer"], origin);
    let other = http
        .get(&metadata)
        .header("Host", "another-application.example")
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), reqwest::StatusCode::BAD_REQUEST);
    let wrong_hostname = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(certificate.pem.as_bytes()).unwrap())
        .resolve("localhost", address)
        .build()
        .unwrap();
    assert!(wrong_hostname
        .get(format!(
            "https://localhost:{}/.well-known/oauth-authorization-server",
            address.port()
        ))
        .send()
        .await
        .is_err());
    // Once the retained signer retires, this already-running listener must not
    // keep advertising the old startup trust snapshot to new clients.
    let rotation = crate::platform::auth::tests::rotate_issuer(
        &engine.server.store().authority,
        &admin,
        NOW + 1,
    )
    .unwrap();
    let response = http.get(&metadata).send().await.unwrap();
    assert!(response.status().is_success());
    let metadata: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        metadata["trellis"]["issuers"][0]["keyId"],
        rotation.next_key_id
    );
    shutdown.cancel();
    server.await.unwrap().unwrap();
}
