//! A real RPC provider whose accepted mutations remain independently observable.

use trellis_rs::__internal::{RuntimeOwner as _, RuntimeService as _};
use runtime_trellis::participants::runtime_trellis_transport_growth_operation_provider::{
    Participant, Provider,
};
use runtime_trellis::types::{Empty, Value};
use std::io::Write as _;
use tokio::io::{AsyncBufReadExt as _, BufReader};

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut runtime = Participant::connect(
        trellis_runtime_acceptance::service_options(
            &std::env::var("TRELLIS_URL")?,
            &std::env::var("TRELLIS_IDENTITY_SEED")?,
        )
        .await?,
    )
    .await?;
    let effects = std::env::var("RPC_EFFECTS_FILE")?;
    let mut provider = Provider::new(&mut runtime);
    let mut api = provider.runtime_trellis_transport_growth_v1();
    api.register_park(move |_, _| {
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&effects)
                .unwrap(),
            "rust"
        )
        .unwrap();
        async {
            Ok(Value {
                value: "rust".into(),
                extra: Default::default(),
            })
        }
    });
    api.register_extend(|_, _| async {
        Ok(Empty {
            extra: Default::default(),
        })
    });
    api.register_progress(|_, _| {
        futures_util::stream::iter([Ok(Value {
            value: "rust".into(),
            extra: Default::default(),
        })])
    });
    let owner = runtime.runtime_native_owner();
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let result = tokio::select! {
        result = runtime.run() => result.map_err(Box::<dyn std::error::Error>::from),
        result = commands.next_line() =>
            result
                    .map(|_| ())
                    .map_err(Box::<dyn std::error::Error>::from),
    };
    owner.shutdown_native_runtime().await?;
    result
}
