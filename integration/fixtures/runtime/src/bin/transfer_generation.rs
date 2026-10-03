use runtime_trellis::apis::runtime_trellis_runtime_v1::rpc::DownloadOutput;
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use std::io::Write as _;
use trellis_rs::service::{FileTransferInfo, ServiceConnectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let mut service = Participant::connect(ServiceConnectOptions::new(&url, &seed)).await?;
    let files = Provider::new(&mut service)
        .files()
        .cloned()
        .ok_or("missing files")?;
    let bytes = (0..131_073)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    files.write("image", bytes).await?;
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
                    &ulid::Ulid::new().to_string(),
                    &expires_at,
                    16_384,
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
