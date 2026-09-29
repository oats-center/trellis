//! Rust transport-generation growth acceptance leg.
//!
//! Connects a generated `TransportGrowthSubject` service, invokes its
//! always-granted `Advance` capability, then waits on stdin so the TypeScript
//! case harness can approve the optional `Extend` capability through real
//! deployment consent and ask for `Extend` on the same logical connection. The
//! harness drives the optional capability and the ordinary-renewal window; the
//! process only prints a bounded line protocol.

use runtime_trellis::participants::runtime_trellis_transport_growth_subject::{
    Participant as SubjectParticipant, Provider as SubjectProvider,
};
use runtime_trellis::types::Empty;
use std::io::Write as _;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use trellis_rs::service::ServiceConnectOptions;

/// Emits one flushable protocol line the harness can act on immediately.
fn emit(line: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("{line}");
    std::io::stdout().flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match run().await {
        Ok(()) => Ok(()),
        Err(error) => {
            emit(&format!("TRANSPORT_GROWTH_ERROR {error}"))?;
            Err(error)
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut runtime = SubjectParticipant::connect(ServiceConnectOptions::new(&url, &seed)).await?;
    let client = SubjectProvider::new(&mut runtime).client();
    let api = client.runtime_trellis_transport_growth_v1();
    emit("TRANSPORT_GROWTH_CONNECTED")?;

    api.advance(&Empty {
        extra: Default::default(),
    })
    .await?;
    emit("TRANSPORT_GROWTH_ADVANCE_OK")?;

    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = commands.next_line().await? {
        match line.trim() {
            "ADVANCE" => {
                api.advance(&Empty {
                    extra: Default::default(),
                })
                .await?;
                emit("TRANSPORT_GROWTH_ADVANCE_OK")?;
            }
            "AVAILABLE" => {
                // Wait until the application layer reflects the approved
                // capability; the transport layer then waits for/uses the
                // adopted generation on the call itself.
                let deadline = Instant::now() + Duration::from_secs(60);
                while !client
                    .availability()
                    .action_runtime_trellis_transport_growth_v1_rpc_extend
                {
                    if Instant::now() >= deadline {
                        return Err("grown capability never became available".into());
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                emit("TRANSPORT_GROWTH_EXTEND_AVAILABLE")?;
            }
            "EXTEND" => {
                api.extend(&Empty {
                    extra: Default::default(),
                })
                .await?;
                emit("TRANSPORT_GROWTH_EXTEND_OK")?;
            }
            "CLOSE" => {
                // Return closes the logical connection: every physical generation
                // is released and the server should drop all attachments.
                emit("TRANSPORT_GROWTH_CLOSED")?;
                break;
            }
            "EXIT" => {
                emit("TRANSPORT_GROWTH_DONE")?;
                break;
            }
            "" => {}
            other => return Err(format!("unknown transport growth command '{other}'").into()),
        }
    }
    Ok(())
}
