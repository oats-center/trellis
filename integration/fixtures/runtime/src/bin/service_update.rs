//! Connect as a later build of the Provider than its instance runs.
//!
//! The Rust service SDK submits the build to Auth and waits until it installs;
//! this prints `service update connected` once it does, then proves the KV the
//! build added is usable.

use runtime_pinned_trellis::participants::runtime_trellis_provider::{
    types::ResourceValue, Participant, Provider,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut runtime =
        Participant::connect(trellis_runtime_acceptance::service_options(&url, &identity).await?)
            .await?;
    println!("service update connected");
    let extended = Provider::new(&mut runtime).client().extended().await?;
    extended
        .put(
            "rust",
            &ResourceValue {
                value: "approved".into(),
                extra: Default::default(),
            },
        )
        .await?;
    let stored = extended.get("rust").await?.ok_or("missing KV value")?;
    println!("service update stored {}", stored.value);
    Ok(())
}
