//! Ordinary key-gated RPC provider: records receipt and waits for a KV release.
use runtime_trellis::participants::runtime_trellis_provider::{
    types::ResourceValue, Participant, Provider,
};
use runtime_trellis::types::{Update, UpdateDetail};
use std::time::Duration;
use trellis_rs::service::{RequestLimits, ServerError, ServiceConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut service = Participant::connect(
        ServiceConnectOptions::new(&url, &identity).with_request_limits(RequestLimits {
            requests: 2,
            bytes: 16 * 1024,
            ..Default::default()
        }),
    )
    .await?;
    let records = Provider::new(&mut service).client().records().await?;
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_echo(move |_, input| {
            let records = records.clone();
            async move {
                records
                    .put(
                        &format!("entered-{}", input.value),
                        &ResourceValue {
                            value: input.value.clone(),
                            extra: Default::default(),
                        },
                    )
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                while records
                    .get(&format!("release-{}", input.value))
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok(input)
            }
        });
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_work(|_, input, op| async move {
            op.progress(Update {
                value: input.value,
                nested: UpdateDetail {
                    count: 1.into(),
                    payload: vec![1].into(),
                    extra: Default::default(),
                },
                extra: Default::default(),
            })
            .await?;
            op.cancellation().cancelled().await;
            Ok(())
        });
    service.run().await?;
    Ok(())
}
