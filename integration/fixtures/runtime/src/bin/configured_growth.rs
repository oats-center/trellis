//! Connect as `ConfiguredGrowthService` and report its live settings.
//!
//! Prints `config level <value>` once connected and again after every live
//! change, so a test can grow the service's authority onto a new transport
//! generation and see whether later changes still arrive.

use runtime_trellis::participants::runtime_trellis_configured_growth_service::{
    Participant, Provider,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut service =
        Participant::connect(trellis_runtime_acceptance::service_options(&url, &identity).await?)
            .await?;
    let mut config = Provider::new(&mut service).config();
    let level = serde_json::to_string(&config.current()?.level)?;
    println!("config level {level}");
    loop {
        let changed = config.changed().await?;
        println!("config level {}", serde_json::to_string(&changed.level)?);
    }
}
