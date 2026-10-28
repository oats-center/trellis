//! Axum HTTP baseline with raw, stream and store modes, optionally over mutual
//! TLS with per-request Ed25519 signatures. Mirrors `benchmarks/runtime/http.ts`.
use crate::{wall_ms, Error, Hasher, Payload};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use futures_util::{StreamExt, TryStreamExt};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWriteExt, BufReader, BufWriter, ReadBuf};
use tokio_util::io::{ReaderStream, StreamReader};
use trellis_rs::service::StoreHandle;

const DOMAIN: &str = "trellis-benchmark-http-request-v1";
const MAX_SKEW_MS: f64 = 60_000.0;
static NONCE: AtomicU64 = AtomicU64::new(0);

/// Coordinator-generated CA, mutual-TLS identities and request-signing key.
pub(crate) struct TlsMaterial {
    pub(crate) ca: Vec<u8>,
    server_cert: Vec<u8>,
    server_key: Vec<u8>,
    client_cert: Vec<u8>,
    client_key: Vec<u8>,
    signing: SigningKey,
}

#[derive(Deserialize)]
struct SigningJwk {
    d: String,
    x: String,
}

impl TlsMaterial {
    pub(crate) async fn load(dir: &FsPath) -> Result<Self, Error> {
        let jwk: SigningJwk =
            serde_json::from_slice(&tokio::fs::read(dir.join("request-signing.json")).await?)?;
        let seed: [u8; 32] = URL_SAFE_NO_PAD
            .decode(jwk.d)?
            .try_into()
            .map_err(|_| "signing seed must be 32 bytes")?;
        let signing = SigningKey::from_bytes(&seed);
        if URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()) != jwk.x {
            return Err("signing key pair mismatch".into());
        }
        Ok(Self {
            ca: tokio::fs::read(dir.join("ca.pem")).await?,
            server_cert: tokio::fs::read(dir.join("server.pem")).await?,
            server_key: tokio::fs::read(dir.join("server.key")).await?,
            client_cert: tokio::fs::read(dir.join("client.pem")).await?,
            client_key: tokio::fs::read(dir.join("client.key")).await?,
            signing,
        })
    }

    /// PEM client certificate followed by its key, as reqwest expects.
    pub(crate) fn client_identity(&self) -> Vec<u8> {
        [
            self.client_cert.as_slice(),
            b"\n",
            self.client_key.as_slice(),
        ]
        .concat()
    }

    /// Headers authorizing one request; `length` is the declared body size.
    pub(crate) fn sign(
        &self,
        method: &str,
        target: &str,
        length: Option<usize>,
    ) -> Vec<(&'static str, String)> {
        let timestamp = format!("{}", wall_ms() as u64);
        // Uniqueness, not secrecy, is what the server's replay check requires.
        let nonce = format!(
            "{}-{}-{}",
            std::process::id(),
            timestamp,
            NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let declared = length.map_or_else(|| "-".to_owned(), |value| value.to_string());
        let signature = self
            .signing
            .sign(canonical(method, target, &declared, &timestamp, &nonce).as_bytes());
        vec![
            ("x-bench-timestamp", timestamp),
            ("x-bench-nonce", nonce),
            (
                "x-bench-signature",
                URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            ),
        ]
    }
}

fn canonical(method: &str, target: &str, length: &str, timestamp: &str, nonce: &str) -> String {
    [DOMAIN, method, target, length, timestamp, nonce].join("\n")
}

struct Verifier {
    key: VerifyingKey,
    seen: Mutex<HashMap<String, f64>>,
}

impl Verifier {
    fn check(&self, method: &Method, uri: &Uri, headers: &HeaderMap) -> Result<(), &'static str> {
        let one = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
        let (Some(timestamp), Some(nonce), Some(signature)) = (
            one("x-bench-timestamp"),
            one("x-bench-nonce"),
            one("x-bench-signature"),
        ) else {
            return Err("missing request proof");
        };
        let now = wall_ms();
        let issued: f64 = timestamp.parse().map_err(|_| "invalid request timestamp")?;
        if (now - issued).abs() > MAX_SKEW_MS {
            return Err("request proof outside skew window");
        }
        let target = uri.path_and_query().map_or("/", |value| value.as_str());
        let declared = one("content-length").unwrap_or("-");
        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| "invalid request proof")?
            .try_into()
            .map_err(|_| "invalid request proof")?;
        self.key
            .verify(
                canonical(method.as_str(), target, declared, timestamp, nonce).as_bytes(),
                &Signature::from_bytes(&signature),
            )
            .map_err(|_| "invalid request proof")?;
        let mut seen = self.seen.lock().map_err(|_| "nonce registry poisoned")?;
        seen.retain(|_, at| now - *at <= 2.0 * MAX_SKEW_MS);
        if seen.insert(nonce.to_owned(), now).is_some() {
            return Err("replayed request proof");
        }
        Ok(())
    }
}

struct HttpState {
    output: PathBuf,
    chunk: usize,
    sizes: Vec<usize>,
    files: StoreHandle,
    verifier: Option<Verifier>,
}

#[derive(Deserialize)]
struct SizeQuery {
    size: usize,
}

type Shared = State<Arc<HttpState>>;

fn rejected(
    state: &HttpState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Option<Response> {
    let verifier = state.verifier.as_ref()?;
    verifier
        .check(method, uri, headers)
        .err()
        .map(|failure| (StatusCode::UNAUTHORIZED, failure).into_response())
}

fn failed(error: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
}

/// AsyncRead that hashes and counts bytes, recording when EOF was observed.
pub(crate) struct HashingReader<R> {
    inner: R,
    hasher: Hasher,
    eof_ms: f64,
}

impl<R> HashingReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Hasher::default(),
            eof_ms: 0.0,
        }
    }

    pub(crate) fn into_hasher(self) -> Hasher {
        self.hasher
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for HashingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(context, buffer);
        if let Poll::Ready(Ok(())) = polled {
            let read = &buffer.filled()[before..];
            if read.is_empty() {
                this.eof_ms = wall_ms();
            } else {
                this.hasher.update(read);
            }
        }
        polled
    }
}

async fn upload(
    State(state): Shared,
    Path(mode): Path<String>,
    Query(query): Query<SizeQuery>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(response) = rejected(&state, &method, &uri, &headers) {
        return response;
    }
    if !state.sizes.contains(&query.size) {
        return (StatusCode::NOT_FOUND, "Unknown payload").into_response();
    }
    let work: Result<serde_json::Value, Error> = async {
        let stream = body.into_data_stream().map_err(std::io::Error::other);
        let reader = StreamReader::new(stream);
        match mode.as_str() {
            "stream" => {
                let mut hasher = Hasher::default();
                tokio::io::copy_buf(
                    &mut BufReader::with_capacity(state.chunk, reader),
                    &mut hasher,
                )
                .await?;
                let at = wall_ms();
                Ok(json!({
                    "size": hasher.size,
                    "digest": hasher.digest(),
                    "verifiedUnixMs": at,
                    "durableUnixMs": at
                }))
            }
            "store" => {
                let mut hashing = HashingReader::new(reader);
                state
                    .files
                    .write_from(&format!("http-upload-{}", query.size), &mut hashing, None)
                    .await?;
                let durable = wall_ms();
                let verified = hashing.eof_ms;
                Ok(json!({
                    "size": hashing.hasher.size,
                    "digest": hashing.hasher.digest(),
                    "verifiedUnixMs": verified,
                    "durableUnixMs": durable
                }))
            }
            "raw" => {
                let path = state.output.join("http-upload.bin");
                let mut file =
                    BufWriter::with_capacity(state.chunk, tokio::fs::File::create(&path).await?);
                tokio::io::copy_buf(
                    &mut BufReader::with_capacity(state.chunk, reader),
                    &mut file,
                )
                .await?;
                file.flush().await?;
                drop(file);
                let durable = wall_ms();
                let mut stored = Hasher::default();
                tokio::io::copy_buf(
                    &mut BufReader::with_capacity(state.chunk, tokio::fs::File::open(path).await?),
                    &mut stored,
                )
                .await?;
                Ok(json!({
                    "size": stored.size,
                    "digest": stored.digest(),
                    "verifiedUnixMs": wall_ms(),
                    "durableUnixMs": durable
                }))
            }
            _ => Err("unknown HTTP mode".into()),
        }
    }
    .await;
    match work {
        Ok(value) => Json(value).into_response(),
        Err(error) => failed(error),
    }
}

async fn download(
    State(state): Shared,
    Path(mode): Path<String>,
    Query(query): Query<SizeQuery>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = rejected(&state, &method, &uri, &headers) {
        return response;
    }
    let size = query.size;
    if !state.sizes.contains(&size) {
        return (StatusCode::NOT_FOUND, "Unknown payload").into_response();
    }
    let chunk = state.chunk;
    let mut builder = Response::builder()
        .header("content-length", size.to_string())
        .header("cache-control", "no-store");
    let body = match mode.as_str() {
        "raw" => {
            match tokio::fs::File::open(state.output.join(format!("http-body-{size}"))).await {
                Ok(file) => Body::from_stream(ReaderStream::with_capacity(file, chunk)),
                Err(error) => return failed(error),
            }
        }
        "stream" => Body::from_stream(ReaderStream::with_capacity(Payload::new(size), chunk)),
        "store" => {
            let metadata = match state.files.metadata(&size.to_string()).await {
                Ok(Some(metadata)) => metadata,
                Ok(None) => return failed("stored object missing"),
                Err(error) => return failed(error),
            };
            if let Some(digest) = metadata.digest {
                builder = builder.header("x-bench-digest", digest);
            }
            let (reader, mut writer) = tokio::io::duplex(2 * chunk);
            let files = state.files.clone();
            tokio::spawn(async move {
                // A read failure truncates the body; the client's length and
                // digest checks then fail the sample.
                let _ = files.read_into(&size.to_string(), &mut writer).await;
                let _ = writer.shutdown().await;
            });
            Body::from_stream(ReaderStream::with_capacity(reader, chunk))
        }
        _ => return (StatusCode::NOT_FOUND, "Unknown mode").into_response(),
    };
    builder.body(body).unwrap_or_else(failed)
}

#[derive(Deserialize)]
struct EchoQuery {
    value: String,
}

/// Serve the HTTP baseline until the process is terminated.
pub(crate) async fn serve(
    output: PathBuf,
    sizes: Vec<usize>,
    chunk: usize,
    files: StoreHandle,
    tls: Option<TlsMaterial>,
) -> Result<(), Error> {
    for size in &sizes {
        let mut file = BufWriter::with_capacity(
            chunk,
            tokio::fs::File::create(output.join(format!("http-body-{size}"))).await?,
        );
        tokio::io::copy(&mut Payload::new(*size), &mut file).await?;
        file.flush().await?;
    }
    let state = Arc::new(HttpState {
        output: output.clone(),
        chunk,
        sizes,
        files,
        verifier: tls.as_ref().map(|tls| Verifier {
            key: tls.signing.verifying_key(),
            seen: Mutex::new(HashMap::new()),
        }),
    });
    let router = Router::new()
        .route(
            "/echo",
            get(
                |State(state): Shared,
                 method: Method,
                 uri: Uri,
                 headers: HeaderMap,
                 Query(values): Query<EchoQuery>| async move {
                    rejected(&state, &method, &uri, &headers)
                        .unwrap_or_else(|| Json(json!({ "value": values.value })).into_response())
                },
            ),
        )
        .route("/download/{mode}", get(download))
        .route("/upload/{mode}", put(upload))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let scheme = if tls.is_some() { "https" } else { "http" };
    tokio::fs::write(
        output.join("http.json"),
        serde_json::to_vec(&json!({
            "httpUrl": format!("{scheme}://{}", listener.local_addr()?),
            "pid": std::process::id(),
            "implementation": "Rust axum/hyper",
            "authentication": if tls.is_some() { "mutual TLS plus per-request Ed25519 signature" } else { "none" },
            "protocol": if tls.is_some() { "HTTP/1.1 over TLS keep-alive" } else { "HTTP/1.1 plaintext keep-alive" },
            "chunkBytes": chunk,
            "modes": {
                "raw": "local file; buffered, closed, not fsynced; verifying re-read",
                "stream": "hashed inline and discarded",
                "store": "hashed inline into the provider's NATS Object Store",
            },
        }))?,
    )
    .await?;
    let Some(tls) = tls else {
        axum::serve(listener, router).await?;
        return Ok(());
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile_certs(&tls.ca)? {
        roots.add(cert)?;
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()?;
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(rustls_pemfile_certs(&tls.server_cert)?, {
            use rustls_pki_types::pem::PemObject as _;
            rustls_pki_types::PrivateKeyDer::from_pem_slice(&tls.server_key)?
        })?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    loop {
        let (tcp, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(tcp).await else {
                return;
            };
            let service = hyper_util::service::TowerToHyperService::new(router);
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await;
        });
    }
}

fn rustls_pemfile_certs(
    pem: &[u8],
) -> Result<Vec<rustls_pki_types::CertificateDer<'static>>, Error> {
    use rustls_pki_types::pem::PemObject as _;
    Ok(rustls_pki_types::CertificateDer::pem_slice_iter(pem).collect::<Result<_, _>>()?)
}

/// Collect a response body through the declared chunk policy, hashing inline.
pub(crate) async fn hash_body(
    response: reqwest::Response,
    started: std::time::Instant,
) -> Result<(Hasher, Option<f64>), Error> {
    let mut hasher = Hasher::default();
    let mut first = None;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if first.is_none() && !chunk.is_empty() {
            first = Some(started.elapsed().as_secs_f64() * 1000.0);
        }
        hasher.update(&chunk);
    }
    Ok((hasher, first))
}
