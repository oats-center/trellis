#[tokio::main]
async fn main() -> miette::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    trellis_cli::app::run().await
}
