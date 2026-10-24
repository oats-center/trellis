//! Same-language native SDK and HTTP performance workloads.
mod http;
mod metrics;
mod raw;
use performance_trellis::apis::performance_trellis_performance_v1::{lives, operations, rpc};
use performance_trellis::apps::performance_trellis_caller::performance_trellis_performance_v1::Client as PerformanceApi;
use performance_trellis::{
    apps::performance_trellis_caller::{Client, Request},
    participants::performance_trellis_provider::{Participant, Provider},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Write as _,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use trellis_rs::{
    auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient},
    client::{OperationState, UserConnectOptions},
    generated::AppRequestDescriptor,
    service::{generate_transfer_id, FileTransferInfo, ServerError, ServiceConnectOptions},
};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    role: String,
    output: PathBuf,
    trellis_url: String,
    service_seed: Option<String>,
    client_id: Option<String>,
    credential_handles: Vec<String>,
    http_url: Option<String>,
    samples: usize,
    calls: usize,
    cpu_ticks: u64,
    sizes: Vec<usize>,
    #[serde(default)]
    warmups: usize,
    #[serde(default)]
    provider_index: usize,
    /// One declared HTTP body chunk policy for clients and servers.
    #[serde(default = "default_http_chunk")]
    http_chunk_bytes: usize,
    /// Mutual-TLS and request-signing material; absent means plaintext.
    http_tls_dir: Option<PathBuf>,
    /// Broker URL and credentials for the raw Core NATS lower bound.
    nats_url: Option<String>,
    nats_creds: Option<PathBuf>,
}
fn default_http_chunk() -> usize {
    64 * 1024
}
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Sample {
    pub(crate) scenario: String,
    pub(crate) transport: &'static str,
    pub(crate) started_unix_ms: u128,
    pub(crate) duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) warmup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) committed_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) setup_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_byte_ms: Option<f64>,
    /// Milliseconds from start until the receiver verified size and digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verified_ms: Option<f64>,
    /// Milliseconds from start until the required durable write completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) durable_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_frame_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) window_frames: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) window_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data_frames: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scheduler_delay_ms: Option<f64>,
}

/// Responsiveness probe: 30 Live first frames or Echo round trips offered at
/// 50 ms intervals, stopping early when `active` reaches zero. Durations start
/// at the offered time, so scheduler delay is included and also recorded.
async fn probe(
    api: &PerformanceApi,
    scenario: &str,
    rpc_probe: bool,
    active: Option<&std::sync::atomic::AtomicUsize>,
) -> Vec<Sample> {
    use futures_util::StreamExt;
    let offered = Instant::now();
    let mut rows = Vec::new();
    for index in 0..30u64 {
        let at = offered + std::time::Duration::from_millis(50 * index);
        tokio::time::sleep_until(at.into()).await;
        if active.is_some_and(|active| active.load(std::sync::atomic::Ordering::Acquire) == 0) {
            break;
        }
        let started = Instant::now();
        let started_unix_ms = now();
        let value = format!("{scenario}-{index}");
        let result: Result<(), Error> = async {
            if rpc_probe {
                let reply = api
                    .echo(&rpc::EchoInput {
                        value: value.clone(),
                        extra: Default::default(),
                    })
                    .await?;
                if reply.value != value {
                    return Err("RPC probe lost data".into());
                }
                return Ok(());
            }
            let mut feed = api
                .watch(&lives::WatchInput {
                    value: value.clone(),
                    extra: Default::default(),
                })
                .await?;
            let first = tokio::time::timeout(std::time::Duration::from_secs(30), feed.next())
                .await?
                .ok_or("Live probe ended before its first frame")??;
            if first.value != value {
                return Err("Live probe lost its first frame".into());
            }
            Ok(())
        }
        .await;
        rows.push(Sample {
            scenario: format!("rust/{scenario}"),
            transport: "trellis",
            started_unix_ms,
            duration_ms: at.elapsed().as_secs_f64() * 1000.0,
            error: result.err().map(|error| error.to_string()),
            scheduler_delay_ms: Some(started.duration_since(at).as_secs_f64() * 1000.0),
            ..Sample::default()
        });
    }
    rows
}
pub(crate) fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_millis()
}
/// Wall clock in fractional milliseconds, comparable across local processes.
pub(crate) fn wall_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_secs_f64()
        * 1000.0
}
pub(crate) struct Payload {
    offset: usize,
    size: usize,
    pattern: Vec<u8>,
}
impl Payload {
    pub(crate) fn new(size: usize) -> Self {
        Self {
            offset: 0,
            size,
            pattern: (0..(64 * 1024 + 250))
                .map(|index| (index % 251) as u8)
                .collect(),
        }
    }
}
impl AsyncRead for Payload {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let count = buffer
            .remaining()
            .min(self.size - self.offset)
            .min(64 * 1024);
        let start = self.offset % 251;
        buffer.put_slice(&self.pattern[start..start + count]);
        self.offset += count;
        Poll::Ready(Ok(()))
    }
}
#[derive(Default)]
pub(crate) struct Hasher {
    hash: Sha256,
    pub(crate) size: u64,
}
impl Hasher {
    pub(crate) fn digest(&self) -> String {
        format!("{:x}", self.hash.clone().finalize())
    }
    pub(crate) fn base64url(&self) -> String {
        base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            self.hash.clone().finalize(),
        )
    }
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.hash.update(bytes);
        self.size += bytes.len() as u64;
    }
}
impl AsyncWrite for Hasher {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.update(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn snapshot_resources(options: &Options) -> Result<Vec<Value>, Error> {
    let mut processes: Vec<Value> =
        serde_json::from_slice(&tokio::fs::read(options.output.join("processes.json")).await?)?;
    processes.push(json!({ "pid": std::process::id(), "role": "load-generator" }));
    let mut rows = Vec::new();
    for process in processes {
        let pid = process["pid"].as_u64().ok_or("process PID missing")?;
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await?;
        let stat: Vec<_> = stat
            .rsplit_once(") ")
            .ok_or("invalid proc stat")?
            .1
            .split_whitespace()
            .collect();
        let memory = tokio::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).await?;
        let mut row = json!({
            "pid": pid,
            "role": process["role"],
            "startTicks": stat[19].parse::<u64>()?,
            "userCpuSeconds": stat[11].parse::<f64>()? / options.cpu_ticks as f64,
            "systemCpuSeconds": stat[12].parse::<f64>()? / options.cpu_ticks as f64
        });
        for (source, key) in [("Rss:", "rssKiB"), ("Pss:", "pssKiB")] {
            row[key] = json!(memory
                .lines()
                .find(|line| line.starts_with(source))
                .ok_or("missing proc memory")?
                .split_whitespace()
                .nth(1)
                .ok_or("invalid proc memory")?
                .parse::<u64>()?);
        }
        rows.push(row);
    }
    Ok(rows)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let options: Options = serde_json::from_str(&input)?;
    let metrics = metrics::install(&options.output, &options.role);
    match options.role.as_str() {
        "provider" | "storage" => {
            let server = discover_native_oauth(&options.trellis_url, &trust_roots()).await?;
            let mut service = Participant::connect(ServiceConnectOptions::new(
                server.nats_servers,
                server.trust,
                options
                    .service_seed
                    .as_deref()
                    .ok_or("service seed missing")?,
            ))
            .await?;
            let files = Provider::new(&mut service)
                .files()
                .cloned()
                .ok_or("store unavailable")?;
            if options.role == "storage" {
                let mut storage_samples = Vec::new();
                for size in &options.sizes {
                    let mut expected = Hasher::default();
                    tokio::io::copy(&mut Payload::new(*size), &mut expected).await?;
                    for index in 0..(options.samples + options.warmups) {
                        for write in [true, false] {
                            let started_unix_ms = now();
                            let started = Instant::now();
                            let key = format!("diagnostic-{size}");
                            if write {
                                files
                                    .write_from(&key, Payload::new(*size), Some(*size as u64))
                                    .await?;
                            }
                            let written_ms = started.elapsed().as_secs_f64() * 1000.0;
                            let mut stored = Hasher::default();
                            files
                                .read_into(&key, &mut stored)
                                .await?
                                .ok_or("diagnostic object missing")?;
                            if stored.size != *size as u64 || stored.digest() != expected.digest() {
                                return Err("corrupt storage diagnostic".into());
                            }
                            storage_samples.push(Sample {
                                scenario: if write {
                                    "rust/backend-write"
                                } else {
                                    "rust/backend-read"
                                }
                                .into(),
                                transport: "store",
                                started_unix_ms,
                                duration_ms: if write {
                                    written_ms
                                } else {
                                    started.elapsed().as_secs_f64() * 1000.0
                                },
                                bytes: Some(*size),
                                warmup: index < options.warmups,
                                ..Sample::default()
                            });
                        }
                    }
                    files.delete(&format!("diagnostic-{size}")).await?;
                }
                tokio::fs::write(
                    options.output.join("native-store-samples.json"),
                    serde_json::to_vec_pretty(&storage_samples)?,
                )
                .await?;
                metrics.shutdown()?;
                return Ok(());
            }
            for size in &options.sizes {
                files
                    .write_from(&size.to_string(), Payload::new(*size), Some(*size as u64))
                    .await?;
            }
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_echo(|_, input| async move { Ok(input) });
            // Live probes: one first frame echoing the input, as in TypeScript.
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_watch(|_, input| futures_util::stream::once(async move { Ok(input) }));
            let downloads = files.clone();
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_download(move |context, input| {
                    let files = downloads.clone();
                    async move {
                        let metadata = files
                            .metadata(&input.value)
                            .await?
                            .ok_or_else(|| ServerError::Nats("stored object missing".into()))?;
                        let expires = (time::OffsetDateTime::now_utc()
                            + time::Duration::minutes(2))
                        .format(&time::format_description::well_known::Rfc3339)
                        .map_err(|e| ServerError::Nats(e.to_string()))?;
                        let plan = context.plan_download_transfer(
                            "files",
                            &generate_transfer_id()
                                .map_err(|error| ServerError::Nats(error.to_string()))?,
                            &expires,
                            FileTransferInfo {
                                key: input.value.clone(),
                                size: metadata.size,
                                updated_at: metadata
                                    .modified_at
                                    .ok_or_else(|| {
                                        ServerError::Nats("modified time missing".into())
                                    })?
                                    .format(&time::format_description::well_known::Rfc3339)
                                    .map_err(|e| ServerError::Nats(e.to_string()))?,
                                digest: metadata
                                    .digest
                                    .ok_or_else(|| ServerError::Nats("digest missing".into()))?,
                                content_type: Some("application/octet-stream".into()),
                                metadata: Default::default(),
                            },
                        )?;
                        let transfer = plan.grant.clone();
                        context
                            .handle()
                            .spawn_download_transfer_endpoint(plan, files)
                            .await?;
                        Ok(rpc::DownloadOutput {
                            response: input,
                            transfer: Some(transfer),
                        })
                    }
                });
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_upload(move |_, input, op| {
                    let files = files.clone();
                    async move {
                        let mut reader = op
                            .open_staged_upload()
                            .await?
                            .ok_or_else(|| ServerError::Nats("staged upload missing".into()))?;
                        let key = format!("upload-{}", input.value);
                        files.write_from(&key, &mut reader, None).await?;
                        let mut stored = Hasher::default();
                        files
                            .read_into(&key, &mut stored)
                            .await?
                            .ok_or_else(|| ServerError::Nats("persisted upload missing".into()))?;
                        op.complete(operations::UploadOutput {
                            value: stored.digest(),
                            extra: Default::default(),
                        })
                        .await?;
                        Ok(())
                    }
                });
            // Stream uploads read the caller's bytes as they arrive: `<size>`
            // hashes and discards them; `store:<size>` writes them once.
            let stream_files = Provider::new(&mut service)
                .files()
                .cloned()
                .ok_or("store unavailable")?;
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_upload_stream(move |_, input, op| {
                    let files = stream_files.clone();
                    async move {
                        let mut stream = op
                            .take_upload_stream()
                            .ok_or_else(|| ServerError::Nats("stream upload missing".into()))?;
                        let mut hashed = Hasher::default();
                        if let Some(size) = input.value.strip_prefix("store:") {
                            let mut tee = http::HashingReader::new(&mut stream);
                            files
                                .write_from(&format!("upload-stream-{size}"), &mut tee, None)
                                .await?;
                            hashed = tee.into_hasher();
                        } else {
                            tokio::io::copy(&mut stream, &mut hashed)
                                .await
                                .map_err(|error| ServerError::Nats(error.to_string()))?;
                        }
                        op.complete(operations::UploadStreamOutput {
                            value: hashed.digest(),
                            extra: Default::default(),
                        })
                        .await?;
                        Ok(())
                    }
                });
            // Stream downloads serve generated bytes with declared metadata.
            let mut declared = HashMap::new();
            for size in &options.sizes {
                let mut hashed = Hasher::default();
                tokio::io::copy(&mut Payload::new(*size), &mut hashed).await?;
                declared.insert(*size, hashed.base64url());
            }
            let declared = Arc::new(declared);
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_download_stream(move |context, input| {
                    let declared = Arc::clone(&declared);
                    async move {
                        let size: usize = input
                            .value
                            .parse()
                            .map_err(|_| ServerError::Nats("invalid size".into()))?;
                        let digest = declared
                            .get(&size)
                            .ok_or_else(|| ServerError::Nats("unknown generated size".into()))?;
                        let format = &time::format_description::well_known::Rfc3339;
                        let now = time::OffsetDateTime::now_utc();
                        let plan = context.plan_stream_download_transfer(
                            &generate_transfer_id()
                                .map_err(|error| ServerError::Nats(error.to_string()))?,
                            &(now + time::Duration::minutes(2))
                                .format(format)
                                .map_err(|e| ServerError::Nats(e.to_string()))?,
                            FileTransferInfo {
                                key: format!("generated-{size}"),
                                size: size as u64,
                                updated_at: now
                                    .format(format)
                                    .map_err(|e| ServerError::Nats(e.to_string()))?,
                                digest: format!("SHA-256={digest}"),
                                content_type: None,
                                metadata: Default::default(),
                            },
                        )?;
                        let transfer = plan.grant.clone();
                        context
                            .handle()
                            .spawn_stream_download_endpoint(plan, Payload::new(size))
                            .await?;
                        Ok(rpc::DownloadStreamOutput {
                            response: input,
                            transfer: Some(transfer),
                        })
                    }
                });
            // Stream-to-store: the staged commit is the only durable write;
            // complete with the runtime-verified digest, no copy or re-read.
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_upload_staged(move |_, _, op| async move {
                    let info = op
                        .upload()
                        .await?
                        .ok_or_else(|| ServerError::Nats("staged upload missing".into()))?;
                    let encoded = info.digest.trim_start_matches("SHA-256=");
                    let digest = base64::Engine::decode(
                        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                        encoded.trim_end_matches('='),
                    )
                    .map_err(|e| ServerError::Nats(e.to_string()))?;
                    op.complete(operations::UploadStagedOutput {
                        value: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
                        extra: Default::default(),
                    })
                    .await?;
                    Ok(())
                });
            tokio::fs::write(
                options
                    .output
                    .join(format!("provider-{}.json", options.provider_index)),
                b"{\"ready\":true}",
            )
            .await?;
            let mut stop =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! { result = service.run() => result?, _ = stop.recv() => {} }
        }
        "http" => {
            // Store mode uses the provider's own `files` store through the same
            // identity; no handlers are registered, so no Trellis traffic lands here.
            let server = discover_native_oauth(&options.trellis_url, &trust_roots()).await?;
            let mut service = Participant::connect(ServiceConnectOptions::new(
                server.nats_servers,
                server.trust,
                options
                    .service_seed
                    .as_deref()
                    .ok_or("service seed missing")?,
            ))
            .await?;
            let files = Provider::new(&mut service)
                .files()
                .cloned()
                .ok_or("store unavailable")?;
            let tls = match &options.http_tls_dir {
                Some(dir) => Some(http::TlsMaterial::load(dir).await?),
                None => None,
            };
            let mut stop =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = http::serve(
                    options.output.clone(),
                    options.sizes.clone(),
                    options.http_chunk_bytes,
                    files,
                    tls
                ) => result?,
                _ = stop.recv() => {}
            }
            drop(service);
        }
        "client" | "resume" => {
            let mut samples = Vec::new();
            let mut windows = Vec::new();
            let started = Instant::now();
            let unix = now();
            let server = discover_native_oauth(&options.trellis_url, &trust_roots()).await?;
            let scope = NativeCredentialScope {
                issuer: server.issuer.clone(),
                instance_id: server.trust.instance_id.clone(),
                account: server.trust.account.clone(),
                client_id: options.client_id.clone().ok_or("client ID missing")?,
                credential_handle: options
                    .credential_handles
                    .first()
                    .ok_or("credential handle missing")?
                    .parse()?,
            };
            let oauth = NativeOAuthClient::open(
                options.output.join("native-credentials"),
                scope.clone(),
                server.trust,
            )
            .await?;
            let session = if options.role == "resume" {
                oauth.resume().await?
            } else {
                let device = oauth
                    .start_device(&json!([{
                        "type": "trellis_capabilities",
                        "required": Request::REQUIRED_CAPABILITIES,
                        "optional": Request::OPTIONAL_CAPABILITIES,
                    }]))
                    .await?;
                println!(
                    "NATIVE_LOGIN {}",
                    json!({
                        "clientId": scope.client_id,
                        "userCode": device.user_code(),
                        "verificationUri": device.verification_uri()
                    })
                );
                std::io::stdout().flush()?;
                device.complete().await?
            };
            let streaming_client = trellis_rs::generated::Client::connect_user(
                UserConnectOptions::new(server.nats_servers, 30_000, session),
            )
            .await?;
            let client = Client::from_generated(streaming_client.clone());
            let api = client.performance_trellis_performance_v1();
            let reply = api
                .echo(&rpc::EchoInput {
                    value: "first".into(),
                    extra: Default::default(),
                })
                .await?;
            if reply.value != "first" {
                return Err("incorrect first response".into());
            }
            samples.push(Sample {
                scenario: if options.role == "resume" {
                    "rust/first-process-resume-first-rpc"
                } else {
                    "rust/first-process-login-first-rpc"
                }
                .into(),
                transport: "trellis",
                started_unix_ms: unix,
                duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                ..Sample::default()
            });
            if options.role == "resume" {
                tokio::fs::write(
                    options.output.join("native-resume-samples.json"),
                    serde_json::to_vec_pretty(&samples)?,
                )
                .await?;
                return Ok(());
            }
            let tls = match &options.http_tls_dir {
                Some(dir) => Some(http::TlsMaterial::load(dir).await?),
                None => None,
            };
            let mut builder = reqwest::Client::builder()
                .http1_only()
                .timeout(std::time::Duration::from_secs(30));
            if let Some(tls) = &tls {
                builder = builder
                    .tls_certs_only([reqwest::Certificate::from_pem(&tls.ca)?])
                    .identity(reqwest::Identity::from_pem(&tls.client_identity())?);
            }
            let http = builder.build()?;
            let http_url = options.http_url.clone().ok_or("HTTP URL missing")?;
            // Every HTTP request carries the same proof headers when TLS is on.
            let signed = |method: reqwest::Method, target: String, length: Option<usize>| {
                let mut request = http.request(method.clone(), format!("{http_url}{target}"));
                if let Some(tls) = &tls {
                    for (name, value) in tls.sign(method.as_str(), &target, length) {
                        request = request.header(name, value);
                    }
                }
                if let Some(length) = length {
                    request = request.header("content-length", length.to_string());
                }
                request
            };
            for transport in ["trellis", "http"] {
                tokio::fs::write(
                    options.output.join("phase.txt"),
                    format!("{transport}/rust/echo"),
                )
                .await?;
                for index in 0..options.calls {
                    let unix = now();
                    let started = Instant::now();
                    let value = format!("echo-{index}");
                    let result: Result<(), Error> = async {
                        let actual = if transport == "trellis" {
                            api.echo(&rpc::EchoInput {
                                value: value.clone(),
                                extra: Default::default(),
                            })
                            .await?
                            .value
                        } else {
                            signed(reqwest::Method::GET, format!("/echo?value={value}"), None)
                                .send()
                                .await?
                                .error_for_status()?
                                .json::<Value>()
                                .await?["value"]
                                .as_str()
                                .ok_or("HTTP value missing")?
                                .to_owned()
                        };
                        if actual != value {
                            return Err("incorrect Echo response".into());
                        }
                        Ok(())
                    }
                    .await;
                    samples.push(Sample {
                        scenario: "rust/echo".into(),
                        transport,
                        started_unix_ms: unix,
                        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                        error: result.err().map(|e| e.to_string()),
                        ..Sample::default()
                    });
                }
            }
            let chunk = options.http_chunk_bytes;
            let mut cases: Vec<(&'static str, String)> = vec![
                ("trellis", "download".into()),
                ("trellis", "download-stream".into()),
                ("trellis", "upload-persisted".into()),
                ("trellis", "upload-staged".into()),
                ("trellis", "upload-stream".into()),
                ("trellis", "upload-stream-store".into()),
            ];
            for mode in ["raw", "stream", "store"] {
                cases.push(("http", format!("download-{mode}")));
                cases.push(("http", format!("upload-{mode}")));
            }
            for size in &options.sizes {
                let mut expected_hash = Hasher::default();
                tokio::io::copy(&mut Payload::new(*size), &mut expected_hash).await?;
                let expected = expected_hash.digest();
                for index in 0..(options.samples + options.warmups) {
                    // Alternate which transport goes first, as the TypeScript lane does.
                    let mut ordered: Vec<_> = cases.iter().collect();
                    if index % 2 == 1 {
                        ordered.sort_by_key(|(transport, _)| *transport != "http");
                    }
                    for (transport, scenario) in ordered {
                        let transport = *transport;
                        let scenario_name = format!("rust/{scenario}");
                        tokio::fs::write(
                            options.output.join("phase.txt"),
                            format!("{transport}/{scenario_name}/{size}"),
                        )
                        .await?;
                        let before = snapshot_resources(&options).await?;
                        let started = Instant::now();
                        let started_wall = wall_ms();
                        let unix = now();
                        let mut sample = Sample {
                            scenario: scenario_name.clone(),
                            transport,
                            started_unix_ms: unix,
                            bytes: Some(*size),
                            warmup: index < options.warmups,
                            ..Sample::default()
                        };
                        let elapsed = |started: Instant| started.elapsed().as_secs_f64() * 1000.0;
                        let result: Result<(), Error> = async {
                            let actual = match (transport, scenario.as_str()) {
                                ("trellis", "download" | "download-stream") => {
                                    let transfer = if scenario == "download" {
                                        api.download(&rpc::DownloadInput {
                                            value: size.to_string(),
                                            extra: Default::default(),
                                        })
                                        .await?
                                        .transfer
                                    } else {
                                        api.download_stream(&rpc::DownloadStreamInput {
                                            value: size.to_string(),
                                            extra: Default::default(),
                                        })
                                        .await?
                                        .transfer
                                    };
                                    sample.setup_ms = Some(elapsed(started));
                                    let mut received = Hasher::default();
                                    streaming_client
                                        .download_transfer_into(
                                            &transfer.ok_or("download grant missing")?,
                                            &mut received,
                                        )
                                        .await?;
                                    if received.size != *size as u64 {
                                        return Err("download length mismatch".into());
                                    }
                                    sample.verified_ms = Some(elapsed(started));
                                    received.digest()
                                }
                                ("trellis", "upload-persisted") => {
                                    let handle = api
                                        .upload()
                                        .start(&operations::UploadInput {
                                            value: format!("{size}-{index}"),
                                            extra: Default::default(),
                                        })
                                        .await?;
                                    handle.upload_from(&mut Payload::new(*size)).await?;
                                    sample.committed_ms = Some(elapsed(started));
                                    sample.verified_ms = sample.committed_ms;
                                    let completed = handle.wait().await?;
                                    if completed.state != OperationState::Completed {
                                        return Err("upload not completed".into());
                                    }
                                    sample.durable_ms = Some(elapsed(started));
                                    completed.output.ok_or("upload result missing")?.value
                                }
                                ("trellis", "upload-staged") => {
                                    let handle = api
                                        .upload_staged()
                                        .start(&operations::UploadStagedInput {
                                            value: format!("{size}-{index}"),
                                            extra: Default::default(),
                                        })
                                        .await?;
                                    handle.upload_from(&mut Payload::new(*size)).await?;
                                    sample.committed_ms = Some(elapsed(started));
                                    sample.verified_ms = sample.committed_ms;
                                    sample.durable_ms = sample.committed_ms;
                                    let completed = handle.wait().await?;
                                    if completed.state != OperationState::Completed {
                                        return Err("upload not completed".into());
                                    }
                                    completed.output.ok_or("upload result missing")?.value
                                }
                                ("trellis", "upload-stream" | "upload-stream-store") => {
                                    let value = if scenario == "upload-stream" {
                                        size.to_string()
                                    } else {
                                        format!("store:{size}")
                                    };
                                    let handle = api
                                        .upload_stream()
                                        .start(&operations::UploadStreamInput {
                                            value,
                                            extra: Default::default(),
                                        })
                                        .await?;
                                    handle.upload_from(&mut Payload::new(*size)).await?;
                                    // Commit is verified delivery to the handler.
                                    sample.committed_ms = Some(elapsed(started));
                                    sample.verified_ms = sample.committed_ms;
                                    let completed = handle.wait().await?;
                                    if completed.state != OperationState::Completed {
                                        return Err("upload not completed".into());
                                    }
                                    if scenario == "upload-stream-store" {
                                        sample.durable_ms = Some(elapsed(started));
                                    }
                                    completed.output.ok_or("upload result missing")?.value
                                }
                                ("http", name) => {
                                    let (direction, mode) =
                                        name.split_once('-').ok_or("invalid HTTP case")?;
                                    if direction == "download" {
                                        let response = signed(
                                            reqwest::Method::GET,
                                            format!("/download/{mode}?size={size}"),
                                            None,
                                        )
                                        .send()
                                        .await?
                                        .error_for_status()?;
                                        sample.setup_ms = Some(elapsed(started));
                                        let declared = response
                                            .headers()
                                            .get("x-bench-digest")
                                            .and_then(|value| value.to_str().ok())
                                            .map(str::to_owned);
                                        let (received, first) =
                                            http::hash_body(response, started).await?;
                                        sample.first_byte_ms = first;
                                        if received.size != *size as u64 {
                                            return Err("HTTP download length mismatch".into());
                                        }
                                        if let Some(declared) = declared {
                                            let decoded = base64::Engine::decode(
                                                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                                                declared
                                                    .trim_start_matches("SHA-256=")
                                                    .trim_end_matches('='),
                                            )?;
                                            let hex: String = decoded
                                                .iter()
                                                .map(|byte| format!("{byte:02x}"))
                                                .collect();
                                            if hex != received.digest() {
                                                return Err("HTTP declared digest mismatch".into());
                                            }
                                        }
                                        sample.verified_ms = Some(elapsed(started));
                                        received.digest()
                                    } else {
                                        let reply = signed(
                                            reqwest::Method::PUT,
                                            format!("/upload/{mode}?size={size}"),
                                            Some(*size),
                                        )
                                        .body(reqwest::Body::wrap_stream(
                                            tokio_util::io::ReaderStream::with_capacity(
                                                Payload::new(*size),
                                                chunk,
                                            ),
                                        ))
                                        .send()
                                        .await?
                                        .error_for_status()?
                                        .json::<Value>()
                                        .await?;
                                        if reply["size"].as_u64() != Some(*size as u64) {
                                            return Err("HTTP upload length mismatch".into());
                                        }
                                        sample.verified_ms = reply["verifiedUnixMs"]
                                            .as_f64()
                                            .map(|at| at - started_wall);
                                        sample.durable_ms = reply["durableUnixMs"]
                                            .as_f64()
                                            .map(|at| at - started_wall);
                                        reply["digest"]
                                            .as_str()
                                            .ok_or("HTTP digest missing")?
                                            .to_owned()
                                    }
                                }
                                _ => return Err("unknown transfer case".into()),
                            };
                            if actual != expected {
                                return Err("transfer digest mismatch".into());
                            }
                            Ok(())
                        }
                        .await;
                        let duration_ms = elapsed(started);
                        sample.duration_ms = duration_ms;
                        sample.error = result.err().map(|e| e.to_string());
                        samples.push(sample);
                        let after = snapshot_resources(&options).await?;
                        if index >= options.warmups {
                            windows.push(json!({
                                "scenario": scenario_name,
                                "transport": transport,
                                "calls": 1,
                                "bytes": size,
                                "durationMs": duration_ms,
                                "before": before,
                                "after": after
                            }));
                        }
                        // Checkpoint raw failures even if a later workload or process fails.
                        tokio::fs::write(
                            options.output.join("native-samples.json"),
                            serde_json::to_vec_pretty(&samples)?,
                        )
                        .await?;
                    }
                }
            }
            // Responsiveness idle and during one concurrent upload and download
            // of the largest object, like the TypeScript transfer lane.
            let size = options.sizes.iter().copied().max().unwrap_or(0);
            let mut expected_hash = Hasher::default();
            tokio::io::copy(&mut Payload::new(size), &mut expected_hash).await?;
            let expected = expected_hash.digest();
            samples.extend(probe(&api, "live-first-frame-idle", false, None).await);
            samples.extend(probe(&api, "rpc-echo-idle", true, None).await);
            tokio::fs::write(
                options.output.join("phase.txt"),
                "trellis/rust/mixed-upload-download",
            )
            .await?;
            let active = std::sync::atomic::AtomicUsize::new(2);
            let started = Instant::now();
            let started_unix_ms = now();
            let upload = async {
                let result: Result<(), Error> = async {
                    let handle = api
                        .upload_staged()
                        .start(&operations::UploadStagedInput {
                            value: "mixed".into(),
                            extra: Default::default(),
                        })
                        .await?;
                    handle.upload_from(&mut Payload::new(size)).await?;
                    active.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                    let completed = handle.wait().await?;
                    if completed.output.ok_or("mixed upload result missing")?.value != expected {
                        return Err("corrupt concurrent upload".into());
                    }
                    Ok(())
                }
                .await;
                if result.is_err() {
                    active.store(0, std::sync::atomic::Ordering::Release);
                }
                result
            };
            let download = async {
                let result: Result<(), Error> = async {
                    let reply = api
                        .download(&rpc::DownloadInput {
                            value: size.to_string(),
                            extra: Default::default(),
                        })
                        .await?;
                    let mut received = Hasher::default();
                    streaming_client
                        .download_transfer_into(
                            &reply.transfer.ok_or("download grant missing")?,
                            &mut received,
                        )
                        .await?;
                    if received.digest() != expected {
                        return Err("corrupt concurrent download".into());
                    }
                    Ok(())
                }
                .await;
                active.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                result
            };
            let probes = async {
                // Probe only while both directions are moving DATA.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let (live, rpc_rows) = tokio::join!(
                    probe(
                        &api,
                        "live-first-frame-under-transfer",
                        false,
                        Some(&active)
                    ),
                    probe(&api, "rpc-echo-under-transfer", true, Some(&active)),
                );
                live.into_iter().chain(rpc_rows).collect::<Vec<_>>()
            };
            let (uploaded, downloaded, probed) = tokio::join!(upload, download, probes);
            samples.extend(probed);
            samples.push(Sample {
                scenario: "rust/mixed-upload-download".into(),
                transport: "trellis",
                started_unix_ms,
                duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                bytes: Some(2 * size),
                error: uploaded
                    .and(downloaded)
                    .err()
                    .map(|error| error.to_string()),
                ..Sample::default()
            });
            if let (Some(url), Some(creds)) = (&options.nats_url, &options.nats_creds) {
                tokio::fs::write(options.output.join("phase.txt"), "raw-core-nats-diagnostic")
                    .await?;
                samples.extend(
                    raw::run(url, creds, &options.sizes, options.samples, options.warmups).await?,
                );
            }
            tokio::fs::write(
                options.output.join("native-samples.json"),
                serde_json::to_vec_pretty(&samples)?,
            )
            .await?;
            tokio::fs::write(
                options.output.join("native-windows.json"),
                serde_json::to_vec_pretty(&windows)?,
            )
            .await?;
            if samples.iter().any(|sample| sample.error.is_some()) {
                return Err("native workload failed; raw samples retained".into());
            }
        }
        _ => return Err("unknown benchmark role".into()),
    }
    metrics.shutdown()?;
    Ok(())
}

/// The installation CA the benchmark driver trusts, passed through the environment.
fn trust_roots() -> Vec<PathBuf> {
    std::env::var_os("TRELLIS_CA_CERTIFICATE")
        .map(PathBuf::from)
        .into_iter()
        .collect()
}
