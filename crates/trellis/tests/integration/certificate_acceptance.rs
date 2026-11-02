use rustls::pki_types::pem::PemObject as _;
use sha2::Digest as _;
use trellis_rs::auth::{discover_accepting_certificate, CertificateAcceptance};

// A program opening two connections at its first start accepts the same
// certificate twice at once; both must connect and leave it remembered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_acceptances_of_one_certificate_both_connect() {
    let runtime = trellis_testkit::TrellisTestRuntime::builder()
        .start()
        .await
        .unwrap();
    let ca = rustls::pki_types::CertificateDer::from_pem_file(&runtime.trust_roots()[0]).unwrap();
    let fingerprint = sha2::Sha256::digest(ca.as_ref())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    let directory = runtime.workdir().join("accepted");
    let acceptance = CertificateAcceptance {
        directory: directory.clone(),
        fingerprint: Some(fingerprint),
        interactive: false,
    };
    for attempt in 0..50 {
        let _ = std::fs::remove_dir_all(&directory);
        let accepting = (0..2).map(|_| {
            let (issuer, acceptance) = (runtime.trellis_url().to_owned(), acceptance.clone());
            tokio::spawn(async move { discover_accepting_certificate(&issuer, &acceptance).await })
        });
        for result in futures_util::future::join_all(accepting).await {
            assert_eq!(
                result
                    .unwrap()
                    .unwrap_or_else(|error| panic!("attempt {attempt}: {error}"))
                    .issuer,
                runtime.trellis_url()
            );
        }
    }
    // Remembered: a later start accepts nothing new.
    let later = CertificateAcceptance {
        fingerprint: Some("00".into()),
        ..acceptance
    };
    discover_accepting_certificate(runtime.trellis_url(), &later)
        .await
        .unwrap();
}
