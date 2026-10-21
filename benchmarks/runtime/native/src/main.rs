//! Same-language native SDK and HTTP performance workloads.
mod metrics;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use performance_trellis::apis::performance_trellis_performance_v1::{operations, rpc};
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
    task::{Context, Poll},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio_util::io::ReaderStream;
use trellis_rs::{
    auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient},
    client::{OperationState, UserConnectOptions},
    generated::AppRequestDescriptor,
    service::{generate_transfer_id, FileTransferInfo, ServerError, ServiceConnectOptions},
};

type Error = Box<dyn std::error::Error + Send + Sync>;
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
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    scenario: String,
    transport: &'static str,
    started_unix_ms: u128,
    duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    warmup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    committed_ms: Option<f64>,
}
fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_millis()
}
struct Payload {
    offset: usize,
    size: usize,
    pattern: Vec<u8>,
}
impl Payload {
    fn new(size: usize) -> Self {
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
struct Hasher {
    hash: Sha256,
    size: u64,
}
impl Hasher {
    fn digest(&self) -> String {
        format!("{:x}", self.hash.clone().finalize())
    }
}
impl AsyncWrite for Hasher {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.hash.update(bytes);
        self.size += bytes.len() as u64;
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
        let mut row = json!({ "pid": pid, "role": process["role"], "startTicks": stat[19].parse::<u64>()?, "userCpuSeconds": stat[11].parse::<f64>()? / options.cpu_ticks as f64, "systemCpuSeconds": stat[12].parse::<f64>()? / options.cpu_ticks as f64 });
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
            let server = discover_native_oauth(&options.trellis_url).await?;
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
                                error: None,
                                warmup: index < options.warmups,
                                committed_ms: None,
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
            for size in &options.sizes {
                let mut file =
                    tokio::fs::File::create(options.output.join(format!("http-body-{size}")))
                        .await?;
                tokio::io::copy(&mut Payload::new(*size), &mut file).await?;
                file.flush().await?;
            }
            let router =
                Router::new()
                    .route(
                        "/echo",
                        get(|Query(values): Query<HashMap<String, String>>| async move {
                            Json(json!({ "value": values.get("value") }))
                        }),
                    )
                    .route(
                        "/download/{size}",
                        get(
                            |State(output): State<PathBuf>, Path(size): Path<usize>| async move {
                                tokio::fs::File::open(output.join(format!("http-body-{size}")))
                                    .await
                                    .map(|file| Body::from_stream(ReaderStream::new(file)))
                                    .map_err(|e| {
                                        (
                                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                            e.to_string(),
                                        )
                                    })
                            },
                        ),
                    )
                    .route(
                        "/upload/{key}",
                        post(
                            |State(output): State<PathBuf>,
                             Path(key): Path<String>,
                             body: Body| async move {
                                 let path = output.join(format!("http-upload-{key}"));
                                 let work: Result<_, Error> = async {
                                     let mut file = tokio::fs::File::create(&path).await?;
                                     let mut stream = body.into_data_stream();
                                     while let Some(chunk) = stream.next().await { file.write_all(&chunk?).await?; }
                                     file.flush().await?;
                                     drop(file);
                                     let mut stored = Hasher::default();
                                     tokio::io::copy(&mut tokio::fs::File::open(path).await?, &mut stored).await?;
                                     Ok(Json(json!({ "value": stored.digest() })))
                                 }.await;
                                 work.map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
                            },
                        ),
                    )
                    .with_state(options.output.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            tokio::fs::write(
                options.output.join("http.json"),
                serde_json::to_vec(
                    &json!({ "httpUrl": format!("http://{}", listener.local_addr()?) }),
                )?,
            )
            .await?;
            axum::serve(listener, router).await?;
        }
        "client" | "resume" => {
            let mut samples = Vec::new();
            let mut windows = Vec::new();
            let started = Instant::now();
            let unix = now();
            let server = discover_native_oauth(&options.trellis_url).await?;
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
                    json!({ "clientId": scope.client_id, "userCode": device.user_code(), "verificationUri": device.verification_uri() })
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
                bytes: None,
                error: None,
                warmup: false,
                committed_ms: None,
            });
            if options.role == "resume" {
                tokio::fs::write(
                    options.output.join("native-resume-samples.json"),
                    serde_json::to_vec_pretty(&samples)?,
                )
                .await?;
                return Ok(());
            }
            let http = reqwest::Client::builder()
                .http1_only()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
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
                            http.get(format!(
                                "{}/echo",
                                options.http_url.as_deref().ok_or("HTTP URL missing")?
                            ))
                            .query(&[("value", &value)])
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
                        bytes: None,
                        error: result.err().map(|e| e.to_string()),
                        warmup: false,
                        committed_ms: None,
                    });
                }
            }
            for size in &options.sizes {
                let mut expected_hash = Hasher::default();
                tokio::io::copy(&mut Payload::new(*size), &mut expected_hash).await?;
                let expected = expected_hash.digest();
                for index in 0..(options.samples + options.warmups) {
                    for transport in ["trellis", "http"] {
                        for upload in [false, true] {
                            let scenario = if upload {
                                "rust/upload-persisted-verified"
                            } else {
                                "rust/download-verified"
                            };
                            tokio::fs::write(
                                options.output.join("phase.txt"),
                                format!("{transport}/{scenario}/{size}"),
                            )
                            .await?;
                            let before = snapshot_resources(&options).await?;
                            let started = Instant::now();
                            let unix = now();
                            let mut committed_ms = None;
                            let result: Result<(), Error> = async {
                                let actual = if upload {
                                    let key = format!("{size}-{index}");
                                    if transport == "trellis" {
                                        let handle = api
                                            .upload()
                                            .start(&operations::UploadInput {
                                                value: key,
                                                extra: Default::default(),
                                            })
                                            .await?;
                                        handle.upload_from(&mut Payload::new(*size)).await?;
                                        committed_ms =
                                            Some(started.elapsed().as_secs_f64() * 1000.0);
                                        let completed = handle.wait().await?;
                                        if completed.state != OperationState::Completed {
                                            return Err("upload not completed".into());
                                        }
                                        completed.output.ok_or("upload result missing")?.value
                                    } else {
                                        http.post(format!(
                                            "{}/upload/{key}",
                                            options
                                                .http_url
                                                .as_deref()
                                                .ok_or("HTTP URL missing")?
                                        ))
                                        .body(reqwest::Body::wrap_stream(ReaderStream::new(
                                            Payload::new(*size),
                                        )))
                                        .send()
                                        .await?
                                        .error_for_status()?
                                        .json::<Value>()
                                        .await?["value"]
                                            .as_str()
                                            .ok_or("HTTP digest missing")?
                                            .to_owned()
                                    }
                                } else if transport == "trellis" {
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
                                    if received.size != *size as u64 {
                                        return Err("download length mismatch".into());
                                    }
                                    received.digest()
                                } else {
                                    let mut response = http
                                        .get(format!(
                                            "{}/download/{size}",
                                            options
                                                .http_url
                                                .as_deref()
                                                .ok_or("HTTP URL missing")?
                                        ))
                                        .send()
                                        .await?
                                        .error_for_status()?;
                                    let mut received = Hasher::default();
                                    while let Some(chunk) = response.chunk().await? {
                                        received.write_all(&chunk).await?;
                                    }
                                    if received.size != *size as u64 {
                                        return Err("HTTP download length mismatch".into());
                                    }
                                    received.digest()
                                };
                                if actual != expected {
                                    return Err("transfer digest mismatch".into());
                                }
                                Ok(())
                            }
                            .await;
                            samples.push(Sample {
                                scenario: scenario.into(),
                                transport,
                                started_unix_ms: unix,
                                duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                                bytes: Some(*size),
                                error: result.err().map(|e| e.to_string()),
                                warmup: index < options.warmups,
                                committed_ms,
                            });
                            let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
                            let after = snapshot_resources(&options).await?;
                            if index >= options.warmups {
                                windows.push(json!({ "scenario": scenario, "transport": transport, "calls": 1, "bytes": size, "durationMs": duration_ms, "before": before, "after": after }));
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
