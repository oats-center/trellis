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
    seed: Option<String>,
    session_id: Option<String>,
    http_url: Option<String>,
    samples: usize,
    calls: usize,
    sizes: Vec<usize>,
    #[serde(default)]
    provider_index: usize,
    #[serde(default)]
    rpc_delay_ms: u64,
    request_limit: usize,
    request_byte_limit: u32,
    #[serde(default)]
    workload: String,
    #[serde(default)]
    arrival_rate: f64,
    #[serde(default)]
    arrival_rates: Vec<f64>,
    #[serde(default)]
    max_outstanding: usize,
    #[serde(default)]
    rpc_value_bytes: usize,
    #[serde(default)]
    warmups: usize,
    cpu_ticks: f64,
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

async fn snapshot_resources(options: &Options) -> Result<Value, Error> {
    let mut processes: Vec<Value> =
        serde_json::from_slice(&tokio::fs::read(options.output.join("processes.json")).await?)?;
    processes.push(json!({"pid": std::process::id(), "role": "load-generator"}));
    let mut rows = Vec::new();
    for process in processes {
        let pid = process["pid"].as_u64().ok_or("invalid process identity")?;
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await?;
        let fields: Vec<_> = stat
            .rsplit_once(") ")
            .ok_or("invalid process stat")?
            .1
            .split_whitespace()
            .collect();
        let memory = tokio::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).await?;
        let mut row = json!({"pid": pid, "role": process["role"],
            "startTicks": fields[19].parse::<u64>()?,
            "userCpuSeconds": fields[11].parse::<f64>()? / options.cpu_ticks,
            "systemCpuSeconds": fields[12].parse::<f64>()? / options.cpu_ticks});
        for (source, field) in [("Rss:", "rssKiB"), ("Pss:", "pssKiB")] {
            let value = memory
                .lines()
                .find_map(|line| line.strip_prefix(source))
                .ok_or("missing memory counter")?
                .split_whitespace()
                .next()
                .ok_or("invalid memory counter")?
                .parse::<u64>()?;
            row[field] = json!(value);
        }
        rows.push(row);
    }
    Ok(json!(rows))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await?;
    let options: Options = serde_json::from_str(&input)?;
    let telemetry =
        trellis_rs::telemetry::init_from_env(trellis_rs::telemetry::TelemetryIdentity::new(
            "trellis-benchmark-native",
            if options.role == "provider" {
                trellis_rs::telemetry::TelemetryRole::Service
            } else {
                trellis_rs::telemetry::TelemetryRole::Cli
            },
            env!("CARGO_PKG_VERSION"),
        ));
    match options.role.as_str() {
        "provider" => {
            let mut service = Participant::connect(
                ServiceConnectOptions::new(
                    &options.trellis_url,
                    options
                        .service_seed
                        .as_deref()
                        .ok_or("service seed missing")?,
                )
                .with_request_limits(trellis_rs::service::RequestLimits {
                    requests: options.request_limit,
                    bytes: options.request_byte_limit,
                    ..Default::default()
                }),
            )
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
                .register_echo(move |_, input| async move {
                    if options.rpc_delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(options.rpc_delay_ms))
                            .await;
                    }
                    Ok(input)
                });
            let records = Provider::new(&mut service).client().records().await?;
            let writes = records.clone();
            let rpc_delay_ms = options.rpc_delay_ms;
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_put_record(move |_, input| {
                    let records = writes.clone();
                    async move {
                        records
                            .put(&input.value, &input)
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                        if rpc_delay_ms > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(rpc_delay_ms))
                                .await;
                        }
                        Ok(input)
                    }
                });
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_read_record(move |_, input| {
                    let records = records.clone();
                    async move {
                        Ok(records
                            .get(&input.value)
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?
                            .ok_or_else(|| {
                                ServerError::Nats(
                                    "Benchmark record missing after acknowledged write".into(),
                                )
                            })?)
                    }
                });
            Provider::new(&mut service)
                .performance_trellis_performance_v1()
                .register_await_cancellation(|_, input, op| async move {
                    op.progress(input).await?;
                    op.cancellation().cancelled().await;
                    Ok(())
                });
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
                if options.workload == "admission" {
                    3_000
                } else {
                    30_000
                },
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
            if options.workload == "admission" {
                if !options.arrival_rate.is_finite()
                    || options.arrival_rate <= 0.0
                    || options.max_outstanding == 0
                    || (!options.arrival_rates.is_empty()
                        && options.arrival_rates.len() != options.samples)
                    || options
                        .arrival_rates
                        .iter()
                        .any(|rate| !rate.is_finite() || *rate <= 0.0)
                {
                    return Err(
                        "admission requires a positive arrival rate and outstanding-call limit"
                            .into(),
                    );
                }
                let mut rows = Vec::new();
                let mut windows = Vec::new();
                for trial in 0..options.warmups + options.samples {
                    let warmup = trial < options.warmups;
                    let phase = trial.saturating_sub(options.warmups);
                    let arrival_rate = options
                        .arrival_rates
                        .get(phase)
                        .copied()
                        .unwrap_or(options.arrival_rate);
                    let value = format!("admission-{trial}");
                    let operation = api
                        .await_cancellation()
                        .start(&operations::AwaitCancellationInput {
                            value: value.clone(),
                            extra: Default::default(),
                        })
                        .await?;
                    tokio::time::timeout(std::time::Duration::from_secs(30), async {
                        loop {
                            if operation
                                .get()
                                .await?
                                .progress
                                .is_some_and(|progress| progress.value == value)
                            {
                                return Ok::<_, Error>(());
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    })
                    .await??;
                    let operation_id = operation.id().to_owned();
                    tokio::fs::write(options.output.join("phase.txt"), "trellis/echo-window")
                        .await?;
                    let before = snapshot_resources(&options).await?;
                    let window_start = tokio::time::Instant::now();
                    let window_unix = now();
                    let mut running = tokio::task::JoinSet::new();
                    let mut cancellation = None;
                    let mut attempted = 0;
                    for index in 0..options.calls {
                        let offset =
                            std::time::Duration::from_secs_f64(index as f64 / arrival_rate);
                        let offered = window_start + offset;
                        loop {
                            tokio::select! {
                                row = running.join_next(), if !running.is_empty() => {
                                    rows.push(row.ok_or("request task disappeared")??);
                                }
                                _ = tokio::time::sleep_until(offered) => break,
                            }
                        }
                        // Reap completed calls before testing the generator's cap.
                        while let Some(row) = running.try_join_next() {
                            rows.push(row?);
                        }
                        if index == options.calls / 2 {
                            let api = api.clone();
                            let id = operation_id.clone();
                            cancellation = Some(tokio::spawn(async move {
                                let unix = now();
                                let started = Instant::now();
                                let result = tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    async {
                                        let operation = api.await_cancellation().control(id)?;
                                        let result = operation.cancel().await?;
                                        if result.state != OperationState::Cancelled {
                                            return Err::<(), Error>(
                                                "Cancellation did not settle".into(),
                                            );
                                        }
                                        Ok(())
                                    },
                                )
                                .await;
                                let mut row = json!({"scenario": "admission-cancel", "transport": "trellis",
                                    "startedUnixMs": unix, "durationMs": started.elapsed().as_secs_f64() * 1000.0,
                                    "warmup": warmup, "phaseIndex": phase});
                                match result {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => row["error"] = json!(format!("{error:?}")),
                                    Err(error) => row["error"] = json!(error.to_string()),
                                }
                                row
                            }));
                        }
                        let offered_unix = window_unix as f64 + offset.as_secs_f64() * 1000.0;
                        let scheduler_delay = tokio::time::Instant::now()
                            .saturating_duration_since(offered)
                            .as_secs_f64()
                            * 1000.0;
                        if running.len() >= options.max_outstanding {
                            rows.push(json!({"scenario": "echo", "transport": "trellis",
                                "startedUnixMs": now(), "offeredUnixMs": offered_unix,
                                "schedulerDelayMs": scheduler_delay, "durationMs": 0.0, "warmup": warmup,
                                "phaseIndex": phase, "loadGeneratorDrop": true, "error": "load generator outstanding-call limit"}));
                            continue;
                        }
                        attempted += 1;
                        let api = api.clone();
                        let value_bytes = options.rpc_value_bytes;
                        running.spawn(async move {
                            let unix = now();
                            let started = Instant::now();
                            let value = format!("echo-{trial}-{index}:{}", "x".repeat(value_bytes));
                            let result = api.echo(&rpc::EchoInput { value: value.clone(), extra: Default::default() }).await;
                            let mut row = json!({"scenario": "echo", "transport": "trellis",
                                "startedUnixMs": unix, "offeredUnixMs": offered_unix,
                                "schedulerDelayMs": scheduler_delay, "durationMs": started.elapsed().as_secs_f64() * 1000.0,
                                "warmup": warmup, "phaseIndex": phase, "bytes": value.len()});
                            match result {
                                Ok(reply) if reply.value == value => {},
                                Ok(_) => row["error"] = json!("incorrect Echo response"),
                                Err(error) => row["error"] = json!(format!("{error:?}")),
                            }
                            row
                        });
                    }
                    while let Some(row) = running.join_next().await {
                        rows.push(row?);
                    }
                    if let Some(task) = cancellation {
                        rows.push(task.await?);
                    }
                    let after = snapshot_resources(&options).await?;
                    windows.push(json!({"scenario": "echo", "transport": "trellis", "warmup": warmup,
                        "before": before, "after": after,
                        "phaseIndex": phase, "startedUnixMs": window_unix,
                        "calls": attempted, "offeredCalls": options.calls, "arrivalRate": arrival_rate,
                        "loadGeneratorDropped": options.calls - attempted,
                        "durationMs": window_start.elapsed().as_secs_f64() * 1000.0}));
                    tokio::fs::write(
                        options.output.join("native-samples.json"),
                        serde_json::to_vec_pretty(&rows)?,
                    )
                    .await?;
                    tokio::fs::write(
                        options.output.join("native-windows.json"),
                        serde_json::to_vec_pretty(&windows)?,
                    )
                    .await?;
                }
                telemetry.shutdown().await;
                if rows.iter().any(|row| row.get("error").is_some()) {
                    return Err("native admission workload failed; raw samples retained".into());
                }
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
    telemetry.shutdown().await;
    Ok(())
}
