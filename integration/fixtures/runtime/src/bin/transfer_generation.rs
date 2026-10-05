use runtime_trellis::apis::runtime_trellis_runtime_v1::rpc::DownloadOutput;
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use std::io::Write as _;
use trellis_rs::service::{generate_transfer_id, FileTransferInfo, ServiceConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::WARN)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let url = std::env::var("TRELLIS_URL")?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut service = Participant::connect(ServiceConnectOptions::new(&url, &seed)).await?;
    let files = Provider::new(&mut service)
        .files()
        .cloned()
        .ok_or("missing files")?;
    let bytes = (0..8 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    files.write("image", bytes).await?;
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_upload(|_, input, op| async move {
            let upload = op.upload().await?.ok_or_else(|| {
                trellis_rs::service::ServerError::Nats("committed upload missing".into())
            })?;
            let mut reader = op.open_staged_upload().await?.ok_or_else(|| {
                trellis_rs::service::ServerError::Nats("staged upload missing".into())
            })?;
            let mut buffer = [0u8; 8192];
            let mut received = 0usize;
            loop {
                let count = tokio::io::AsyncReadExt::read(&mut reader, &mut buffer)
                    .await
                    .map_err(|error| trellis_rs::service::ServerError::Nats(error.to_string()))?;
                if count == 0 {
                    break;
                }
                if buffer[..count]
                    .iter()
                    .enumerate()
                    .any(|(index, byte)| *byte != ((received + index) % 251) as u8)
                {
                    return Err(trellis_rs::service::ServerError::Nats(
                        "staged upload bytes differ".into(),
                    ));
                }
                received += count;
            }
            if received != 8 * 1024 * 1024 || received as u64 != upload.size {
                return Err(trellis_rs::service::ServerError::Nats(
                    "staged upload size differs".into(),
                ));
            }
            op.complete(input).await?;
            Ok(())
        });
    Provider::new(&mut service)
        .runtime_trellis_runtime_v1()
        .register_download(move |context, input| {
            let files = files.clone();
            async move {
                let metadata = files.metadata("image").await?.expect("stored image");
                let expires_at = (time::OffsetDateTime::now_utc() + time::Duration::minutes(2))
                    .format(&time::format_description::well_known::Rfc3339)
                    .expect("timestamp");
                let plan = context.plan_download_transfer(
                    "files",
                    &generate_transfer_id().map_err(|error| {
                        trellis_rs::service::ServerError::Nats(error.to_string())
                    })?,
                    &expires_at,
                    FileTransferInfo {
                        key: "image".into(),
                        size: metadata.size,
                        updated_at: metadata
                            .modified_at
                            .expect("modified at")
                            .format(&time::format_description::well_known::Rfc3339)
                            .expect("timestamp"),
                        digest: metadata.digest.expect("digest"),
                        content_type: Some("application/octet-stream".into()),
                        metadata: Default::default(),
                    },
                )?;
                let grant = plan.grant.clone();
                context
                    .handle()
                    .spawn_download_transfer_endpoint(plan, files)
                    .await?;
                Ok(DownloadOutput {
                    response: input,
                    transfer: Some(grant.into()),
                })
            }
        });
    println!("transfer provider ready");
    std::io::stdout().flush()?;
    service.run().await?;
    Ok(())
}
