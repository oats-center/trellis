//! Rust caller leg for the `liveprobe@v1` acceptance scenario.
//!
//! Drives one busy Feed (1,025 ordered frames, then a sentinel 1,026) and one
//! quiet Feed (idle across heartbeat cycles, then one released frame) on a
//! single caller connection, and prints a bounded JSON summary the case
//! harness asserts against.

use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_liveprobe_v1::{lives, rpc};
use runtime_trellis::participants::runtime_trellis_live_probe_caller::Client;
use runtime_trellis::types::LiveProbeFrame;
use runtime_trellis::Int64;
use std::io::Write as _;
use std::time::Duration;
use trellis_rs::auth::{load_admin_session, start_agent_login, StartAgentLoginOpts};
use trellis_rs::client::{UserConnectOptions, UserSessionCredentials};

const BUSY_TOTAL: i64 = 1_026;
const TWO_HEARTBEATS: Duration = Duration::from_millis(21_000);

async fn next_frame<S>(
    stream: &mut S,
    expected: i64,
    label: &str,
) -> Result<LiveProbeFrame, Box<dyn std::error::Error>>
where
    S: futures_util::Stream<Item = Result<LiveProbeFrame, trellis_rs::client::TrellisClientError>>
        + Unpin,
{
    let frame = stream
        .next()
        .await
        .ok_or_else(|| format!("{label} ended before frame {expected}"))??;
    if frame.index.0 != expected {
        return Err(format!(
            "{label} frame {} out of order (expected {expected})",
            frame.index.0
        )
        .into());
    }
    Ok(frame)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let login = start_agent_login(&StartAgentLoginOpts {
        trellis_url: &url,
        participant_id: "runtime-trellis.LiveProbeCaller",
        allow_insecure_origin: false,
    })
    .await?;
    println!("liveprobe login {}", login.login_url());
    std::io::stdout().flush()?;
    let outcome = login.complete_without_persistence(&url).await?;
    trellis_rs::auth::save_admin_session(&outcome.state)?;
    let session = load_admin_session()?;
    let client = Client::connect(UserConnectOptions::new(
        &url,
        5_000,
        UserSessionCredentials {
            login_session_id: &session.login_session_id,
            session_key_seed_base64url: &session.session_seed,
        },
        "runtime-trellis.LiveProbeCaller",
    ))
    .await?;
    let api = client.runtime_trellis_liveprobe_v1();
    let run_id = format!("v3-{}", std::process::id());
    let mut busy = api
        .watch(&lives::WatchInput {
            run_id: run_id.clone(),
            stream_id: "busy".to_owned(),
        })
        .await?;
    let quiet_stream = api
        .watch(&lives::WatchInput {
            run_id: run_id.clone(),
            stream_id: "quiet".to_owned(),
        })
        .await?;
    // Activate the quiet Feed immediately; an unpolled prepared handle expires
    // after the reservation window even though no data is expected yet.
    let quiet_task = tokio::spawn(async move {
        let mut stream = quiet_stream;
        let mut indexes = Vec::new();
        while let Some(item) = stream.next().await {
            indexes.push(item?.index.0);
        }
        Ok::<Vec<i64>, trellis_rs::client::TrellisClientError>(indexes)
    });

    api.release(&rpc::ReleaseInput {
        run_id: run_id.clone(),
        stream_id: "busy".to_owned(),
        through_index: Int64(1_025),
        finish: false,
        fail: false,
        payload_bytes: None,
        padding_bytes: None,
    })
    .await?;
    for expected in 1..=1_025 {
        next_frame(&mut busy, expected, "busy").await?;
    }

    // Let the quiet Feed cross two real post-activation heartbeat cycles while
    // it still has nothing to deliver.
    tokio::time::sleep(TWO_HEARTBEATS).await;

    api.release(&rpc::ReleaseInput {
        run_id: run_id.clone(),
        stream_id: "quiet".to_owned(),
        through_index: Int64(1),
        finish: true,
        fail: false,
        payload_bytes: None,
        padding_bytes: None,
    })
    .await?;
    let quiet_indexes = quiet_task.await??;
    if quiet_indexes != vec![1] {
        return Err(format!("quiet Feed delivered {quiet_indexes:?}, expected [1]").into());
    }

    api.release(&rpc::ReleaseInput {
        run_id: run_id.clone(),
        stream_id: "busy".to_owned(),
        through_index: Int64(BUSY_TOTAL),
        finish: true,
        fail: false,
        payload_bytes: None,
        padding_bytes: None,
    })
    .await?;
    next_frame(&mut busy, BUSY_TOTAL, "busy").await?;
    if busy.next().await.transpose()?.is_some() {
        return Err("busy Feed produced a frame past its released range".into());
    }

    let busy_status = api
        .inspect(&rpc::InspectInput {
            run_id: run_id.clone(),
            stream_id: "busy".to_owned(),
        })
        .await?;
    let quiet_status = api
        .inspect(&rpc::InspectInput {
            run_id: run_id.clone(),
            stream_id: "quiet".to_owned(),
        })
        .await?;
    println!(
        "LIVE_PROBE_CALLER_RESULT {{\"busyFrames\":{},\"quietFrames\":1,\"busyStarts\":{},\"busyCleanups\":{},\"busyActive\":{},\"busyEmitted\":{},\"quietStarts\":{},\"quietCleanups\":{},\"quietActive\":{},\"quietEmitted\":{}}}",
        BUSY_TOTAL,
        busy_status.starts.0,
        busy_status.cleanups.0,
        busy_status.active.0,
        busy_status.emitted.0,
        quiet_status.starts.0,
        quiet_status.cleanups.0,
        quiet_status.active.0,
        quiet_status.emitted.0,
    );
    std::io::stdout().flush()?;
    Ok(())
}
