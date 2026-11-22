//! A Rust service receives its webhook endpoints' accepted requests as events
//! and answers decide calls through the generated endpoint handles.
use std::time::Duration;

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_webhook_service::{Participant, Provider};
use serde_json::json;
use trellis_rs::client::{webhook_hmac, webhook_response, WebhookHmacAlgorithm};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rust_service_receives_webhook_events_and_answers_decide_calls() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("billing")
        .await
        .unwrap();
    runtime
        .set_config(
            &identity,
            json!({ "partnerUser": "partner", "relayRanges": ["127.0.0.0/8", "::1/128"] }),
            json!({
                "signing": "signing-secret",
                "hookToken": "right-token-value",
                "partnerPassword": "partner-password",
            }),
        )
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let provider = Provider::new(&mut service);
    let mut deliveries = provider.token().unwrap().subscribe().await.unwrap();
    let _decide = provider
        .handshake()
        .unwrap()
        .decide(|request| async move {
            let body = String::from_utf8_lossy(&request.delivery.body.0).into_owned();
            Ok::<_, String>(
                serde_json::from_value(json!({
                    "verdict": if body == "setup" { "ignore" } else { "accept" },
                    "response": webhook_response(200, Some("text/plain"), b"challenge-echo"),
                }))
                .unwrap(),
            )
        })
        .await
        .unwrap();
    let urls = runtime.webhook_urls(&identity).await.unwrap();
    let http = runtime.https_client_builder().build().unwrap();

    // An accepted request arrives with its body byte for byte.
    let body = b"{\"id\": 1,  \"kept\": \"as sent\"}";
    let response = http
        .post(&urls["token"])
        .header("x-hook-token", "right-token-value")
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = tokio::time::timeout(Duration::from_secs(20), deliveries.next())
        .await
        .expect("the Received event arrives")
        .unwrap()
        .unwrap();
    assert_eq!(delivery.body.0, body);
    assert_eq!(delivery.endpoint, "token");

    // The decide handler's answer goes back to the sender in sync mode.
    let signature = webhook_hmac(WebhookHmacAlgorithm::Sha256, b"signing-secret", b"setup")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let handshake = http
        .post(&urls["handshake"])
        .header("x-body-signature", format!("sha256={signature}"))
        .body("setup")
        .send()
        .await
        .unwrap();
    assert_eq!(handshake.status(), reqwest::StatusCode::OK);
    assert_eq!(handshake.text().await.unwrap(), "challenge-echo");
    runtime.shutdown().await.unwrap();
}
