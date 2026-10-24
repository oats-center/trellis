//! NX03: finite empty Watch completes with no application frames.
use futures_util::StreamExt;
use runtime_trellis::apis::runtime_trellis_runtime_v1::lives;
use runtime_trellis::apps::runtime_trellis_caller::{Client, Request};
use std::io::Write as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::from_generated(
        trellis_rs::generated::Client::connect_user(
            trellis_runtime_acceptance::user_options::<Request>(8_000).await?,
        )
        .await?,
    );
    let mut frames = client
        .runtime_trellis_runtime_v1()
        .watch(&lives::WatchInput {
            extra: Default::default(),
        })
        .await?;
    if frames.next().await.is_some() {
        return Err("empty Watch emitted an application frame".into());
    }
    println!("empty watch complete");
    std::io::stdout().flush()?;
    Ok(())
}
