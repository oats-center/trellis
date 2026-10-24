//! Generated application-major interoperability over the one Live protocol.

use trellis_rs::__internal::{RuntimeOwner as _, RuntimeService as _};
use futures_util::{stream, StreamExt};
use runtime_live_v2::apis::runtime_trellis_documents_v2::lives as v2_lives;
use runtime_live_v2::participants::runtime_trellis_live_v2_provider::{Participant, Provider};
use runtime_trellis::apis::runtime_trellis_documents_v1::lives as v1_lives;
use runtime_trellis::apps::runtime_trellis_live_v1_caller::Request;
use runtime_trellis::types::Value;
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt, BufReader};
use trellis_rs::generated::Client;

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|argument| argument == "--caller") {
        let client =
            Client::connect_user(trellis_runtime_acceptance::user_options::<Request>(5_000).await?)
                .await?;
        let denied = client
            .live::<v2_lives::Watch>(&runtime_live_v2::types::Value {
                value: "foreign-major".into(),
                extra: Default::default(),
            })
            .await;
        assert!(
            denied.is_err(),
            "the first-major grant must not open the second major"
        );
        let mut observation = client
            .live::<v1_lives::Watch>(&Value {
                value: "rust".into(),
                extra: Default::default(),
            })
            .await?;
        let mut values = Vec::new();
        while let Some(value) = observation.next().await {
            values.push(value?.value);
        }
        println!("LIVE_VERSION_RESULT {}", serde_json::to_string(&values)?);
        std::io::stdout().flush()?;
        return Ok(());
    }

    let mut service = Participant::connect(
        trellis_runtime_acceptance::service_options(
            &std::env::var("TRELLIS_URL")?,
            &std::env::var("TRELLIS_IDENTITY_SEED")?,
        )
        .await?,
    )
    .await?;
    let mut provider = Provider::new(&mut service);
    provider
        .runtime_trellis_documents_v2()
        .register_echo(|_, input| async move { Ok(input) });
    provider
        .runtime_trellis_documents_v2()
        .register_watch(|_, mut input| {
            input.value = format!("v2:{}", input.value);
            stream::iter([Ok(input)])
        });
    provider
        .runtime_trellis_documents_v2()
        .register_list_watch(|_, mut input| {
            input.value = format!("v2:list:{}", input.value);
            stream::iter([Ok(input)])
        });
    provider
        .runtime_trellis_documents_v2()
        .register_work(|_, mut input, operation| async move {
            input.value = format!("v2:work:{}", input.value);
            operation.complete(input).await?;
            Ok(())
        });
    let owner = service.runtime_native_owner();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let result = tokio::select! {
        result = service.run() => result.map_err(Box::<dyn std::error::Error>::from),
        line = lines.next_line() => line.map(|_| ()).map_err(Box::<dyn std::error::Error>::from),
    };
    owner.shutdown_native_runtime().await?;
    result
}
