//! Connect as `ConfiguredService` with a settings check and an API-key client
//! that fail the ways an author's code can, quoting the key.
//!
//! The check fails with an error quoting the key, and its base64, when the
//! key starts with `check-throws-`; refuses `region` with a reason quoting the
//! candidate key and the key in effect when the region is `explicit`;
//! fails with an error quoting the unchanged key, and its base64, when the
//! region is `plain`; and refuses `region` with a reason longer than Auth
//! keeps when it is `long`. The client's builder fails with an error quoting
//! the key, and its base64, when the key starts with `build-throws-`. Prints
//! `READY` to stderr once its check and client are registered, then serves.

use base64::Engine as _;
use runtime_trellis::participants::runtime_trellis_configured_service::{
    Config, Participant, Provider,
};
use trellis_rs::generated::Nullable;
use trellis_rs::service::Rejection;

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
    let config = Provider::new(&mut service).config();
    let encoded = |key: &str| base64::engine::general_purpose::STANDARD.encode(key);
    let in_effect = config.clone();
    config
        .check(move |candidate: &Config| {
            if candidate.api_key.starts_with("check-throws-") {
                Err(std::io::Error::other(format!(
                    "the endpoint refused {} ({})",
                    candidate.api_key,
                    encoded(&candidate.api_key)
                )))?;
            }
            match &candidate.region {
                Nullable::Value(region) if region == "explicit" => {
                    let was = in_effect.current().map(|now| now.api_key);
                    Err(Rejection::new(
                        ["region"],
                        format!(
                            "region conflicts with key {}, was {}",
                            candidate.api_key,
                            was.unwrap_or_default()
                        ),
                    ))
                }
                Nullable::Value(region) if region == "plain" => {
                    Err(std::io::Error::other(format!(
                        "region plain is not served for {} ({})",
                        candidate.api_key,
                        encoded(&candidate.api_key)
                    )))?
                }
                Nullable::Value(region) if region == "long" => {
                    Err(Rejection::new(["region"], "é".repeat(1000)))
                }
                _ => Ok(()),
            }
        })
        .await?;
    let _client = config
        .derive(["api_key"], move |candidate: &Config| {
            if candidate.api_key.starts_with("build-throws-") {
                Err(std::io::Error::other(format!(
                    "cannot sign in with {} ({})",
                    candidate.api_key,
                    encoded(&candidate.api_key)
                )))?;
            }
            Ok(candidate.api_key.clone())
        })
        .await?;
    eprintln!("READY");
    service.run().await?;
    Ok(())
}
