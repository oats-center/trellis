//! Network-enabled acceptance for the explicitly selected pinned NATS download.
//!
//! This is a separate test target so `--test live` never performs a network
//! download. Run it deliberately after network prerequisites are available:
//! `cargo test --test download`.

use trellis_testkit::{NatsSource, TrellisTestRuntime};

/// T25: `DownloadPinned` delegates to the real server's verified acquisition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t25_download_pinned_acquires_verified_nats() {
    let mut runtime = TrellisTestRuntime::builder()
        .nats(NatsSource::DownloadPinned)
        .start()
        .await
        .expect("start runtime with the pinned NATS download");
    runtime.shutdown().await.expect("shutdown runtime");
}
