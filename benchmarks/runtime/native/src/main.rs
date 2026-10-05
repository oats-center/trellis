//! Same-language native SDK and HTTP performance workloads.
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use performance_trellis::apis::performance_trellis_performance_v1::{operations, rpc};
use performance_trellis::participants::{
    performance_trellis_caller::Client,
    performance_trellis_provider::{Participant, Provider},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;
use trellis_rs::{
    client::{OperationState, UserConnectOptions, UserSessionCredentials},
    service::{FileTransferInfo, ServerError, ServiceConnectOptions},
};

type Error = Box<dyn std::error::Error + Send + Sync>;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    role: String,
    output: PathBuf,
    trellis_url: String,
    service_seed: Option<String>,
    seed: Option<String>,
    session_id: Option<String>,
    http_url: Option<String>,
    samples: usize,
    calls: usize,
    sizes: Vec<usize>,
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
}
fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_millis()
}
fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let options: Options = serde_json::from_str(&input)?;
    match options.role.as_str() {
        "provider" => {
            let mut service = Participant::connect(ServiceConnectOptions::new(
                &options.trellis_url,
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
            for size in &options.sizes {
                files.write(&size.to_string(), payload(*size)).await?;
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
                            &ulid::Ulid::new().to_string(),
                            &expires,
                            16384,
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
                            transfer: Some(transfer.into()),
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
                        let mut body = Vec::new();
                        reader
                            .read_to_end(&mut body)
                            .await
                            .map_err(|e| ServerError::Nats(e.to_string()))?;
                        let key = format!("upload-{}", input.value);
                        files.write(&key, body).await?;
                        let stored = files
                            .read(&key)
                            .await?
                            .ok_or_else(|| ServerError::Nats("persisted upload missing".into()))?;
                        op.complete(operations::UploadOutput {
                            value: digest(&stored),
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
            service.run().await?;
        }
        "http" => {
            for size in &options.sizes {
                tokio::fs::write(
                    options.output.join(format!("http-body-{size}")),
                    payload(*size),
                )
                .await?;
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
                                tokio::fs::read(output.join(format!("http-body-{size}")))
                                    .await
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
                             body: Bytes| async move {
                                let path = output.join(format!("http-upload-{key}"));
                                tokio::fs::write(&path, body).await.map_err(|e| {
                                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                                })?;
                                let stored = tokio::fs::read(path).await.map_err(|e| {
                                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                                })?;
                                Ok::<_, (axum::http::StatusCode, String)>(Json(
                                    json!({ "value": digest(&stored) }),
                                ))
                            },
                        ),
                    )
                    .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
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
        "client" => {
            let mut samples = Vec::new();
            let started = Instant::now();
            let unix = now();
            let client = Client::connect(UserConnectOptions::new(
                &options.trellis_url,
                30_000,
                UserSessionCredentials {
                    login_session_id: options
                        .session_id
                        .as_deref()
                        .ok_or("login session missing")?,
                    session_key_seed_base64url: options.seed.as_deref().ok_or("seed missing")?,
                },
                "performance-trellis.Caller",
            ))
            .await?;
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
                scenario: "rust/first-process-resume-first-rpc".into(),
                transport: "trellis",
                started_unix_ms: unix,
                duration_ms: started.elapsed().as_secs_f64() * 1000.0,
                bytes: None,
                error: None,
            });
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
                    });
                }
            }
            for size in &options.sizes {
                let body = payload(*size);
                let expected = digest(&body);
                for index in 0..options.samples {
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
                            let started = Instant::now();
                            let unix = now();
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
                                        handle.upload(&body).await?;
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
                                        .body(body.clone())
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
                                    let bytes = client
                                        .download_transfer(
                                            &reply.transfer.ok_or("download grant missing")?,
                                        )
                                        .await?;
                                    if bytes.len() != *size {
                                        return Err("download length mismatch".into());
                                    }
                                    digest(&bytes)
                                } else {
                                    let bytes = http
                                        .get(format!(
                                            "{}/download/{size}",
                                            options
                                                .http_url
                                                .as_deref()
                                                .ok_or("HTTP URL missing")?
                                        ))
                                        .send()
                                        .await?
                                        .error_for_status()?
                                        .bytes()
                                        .await?;
                                    if bytes.len() != *size {
                                        return Err("HTTP download length mismatch".into());
                                    }
                                    digest(&bytes)
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
                            });
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
            if samples.iter().any(|sample| sample.error.is_some()) {
                return Err("native workload failed; raw samples retained".into());
            }
        }
        _ => return Err("unknown benchmark role".into()),
    }
    Ok(())
}
