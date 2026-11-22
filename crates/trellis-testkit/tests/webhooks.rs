//! A Rust service receives its webhook endpoints' accepted requests as events,
//! reads their bodies through the body views, answers decide calls through
//! the generated endpoint handles, and reports each endpoint's state in its
//! health.
use std::time::Duration;

use futures_util::StreamExt as _;
use runtime_trellis::participants::runtime_trellis_webhook_service::{Participant, Provider};
use serde_json::{json, Value};
use trellis_rs::client::{
    webhook_hmac, webhook_response, WebhookBody as _, WebhookBodyError, WebhookHmacAlgorithm,
};
use trellis_runtime_apis::apis::trellis_health_v1;
use trellis_runtime_apis::types::{
    HealthInspectRequest, HealthInspectRequestContractId, HealthInspectRequestParticipantKind,
};
use trellis_testkit::TrellisTestRuntime;

/// The next accepted request on an endpoint's stream.
async fn next<T, E: std::fmt::Debug>(
    deliveries: &mut (impl futures_util::Stream<Item = Result<T, E>> + Unpin),
) -> T {
    tokio::time::timeout(Duration::from_secs(20), deliveries.next())
        .await
        .expect("the Received event arrives")
        .expect("the stream stays open")
        .unwrap()
}

/// A decide failure whose `Display` panics, as a faulty application error may.
struct Unprintable;

impl std::fmt::Display for Unprintable {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("the decide error cannot be printed")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rust_service_receives_webhook_events_and_answers_decide_calls() {
    // A service logs, so its failure lines are formatted as in production.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("error"))
        .with_test_writer()
        .try_init();
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("billing")
        .await
        .unwrap();
    // The relay's optional secret stays unset.
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
    let mut service = Participant::connect(
        identity
            .connect_options()
            .with_health_heartbeat_interval_ms(1_000),
    )
    .await
    .unwrap();
    let provider = Provider::new(&mut service);
    let mut deliveries = provider.token().unwrap().subscribe().await.unwrap();
    let _decide = provider
        .handshake()
        .unwrap()
        .decide(|request| async move {
            // The decide request's delivery has the same body views.
            if request
                .delivery
                .text()
                .is_ok_and(|body| body == "unprintable")
            {
                return Err(Unprintable);
            }
            assert!(
                !request.delivery.text().is_ok_and(|body| body == "panic"),
                "the decide handler panics"
            );
            let setup = request.delivery.text().is_ok_and(|body| body == "setup");
            Ok(serde_json::from_value(json!({
                "verdict": if setup { "ignore" } else { "accept" },
                "response": webhook_response(200, Some("text/plain"), b"challenge-echo"),
            }))
            .unwrap())
        })
        .await
        .unwrap();
    let urls = runtime.webhook_urls(&identity).await.unwrap();
    let http = runtime.https_client_builder().build().unwrap();
    let send = |body: Vec<u8>| {
        http.post(&urls["token"])
            .header("x-hook-token", "right-token-value")
            .body(body)
            .send()
    };

    // A sender's client that trusts only the runtime's CA reaches the
    // endpoint (405: it takes POST); one that trusts another CA does not.
    let only = |pem: &[u8]| {
        reqwest::Client::builder()
            .tls_certs_only([reqwest::Certificate::from_pem(pem).unwrap()])
            .build()
            .unwrap()
    };
    let runtime_ca = std::fs::read(&runtime.trust_roots()[0]).unwrap();
    let reached = only(&runtime_ca).get(&urls["token"]).send().await.unwrap();
    assert_eq!(reached.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    let mut other = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    other.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let other = other
        .self_signed(&rcgen::KeyPair::generate().unwrap())
        .unwrap();
    let refused = only(other.pem().as_bytes())
        .get(&urls["token"])
        .send()
        .await
        .unwrap_err();
    assert!(refused.is_connect(), "{refused:?}");
    assert!(
        format!("{refused:?}").contains("UnknownIssuer"),
        "{refused:?}"
    );

    // An accepted request arrives with its body byte for byte, and the views
    // read those bytes as text and JSON.
    let body = b"{\"id\": 1,  \"kept\": \"as sent\"}";
    let response = send(body.to_vec()).await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let delivery = next(&mut deliveries).await;
    assert_eq!(delivery.body.0, body);
    assert_eq!(delivery.endpoint, "token");
    assert_eq!(delivery.text().unwrap().as_bytes(), body);
    assert_eq!(
        delivery.json::<Value>().unwrap(),
        json!({ "id": 1, "kept": "as sent" })
    );

    // A form body reads as its fields; bytes that are not UTF-8 are kept as
    // sent, and the text and form views refuse them.
    let form = b"MessageSid=SM1&Body=hi+there".to_vec();
    let binary = vec![0xff, 0xfe, b'{'];
    for body in [form, binary] {
        let response = send(body).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }
    let (first, second) = (next(&mut deliveries).await, next(&mut deliveries).await);
    let (form, binary) = if first.body.0.starts_with(b"Message") {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(
        form.form().unwrap(),
        [
            ("MessageSid".to_owned(), "SM1".to_owned()),
            ("Body".to_owned(), "hi there".to_owned())
        ]
    );
    assert!(matches!(
        form.json::<Value>(),
        Err(WebhookBodyError::NotJson(_))
    ));
    assert_eq!(binary.body.0, [0xff, 0xfe, b'{']);
    assert!(matches!(binary.text(), Err(WebhookBodyError::NotText(_))));
    assert!(matches!(binary.form(), Err(WebhookBodyError::NotText(_))));

    // The decide handler's answer goes back to the sender in sync mode.
    let handshake = |body: &'static str| {
        let signature = webhook_hmac(
            WebhookHmacAlgorithm::Sha256,
            b"signing-secret",
            body.as_bytes(),
        )
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
        http.post(&urls["handshake"])
            .header("x-body-signature", format!("sha256={signature}"))
            .body(body)
            .send()
    };
    let answered = handshake("setup").await.unwrap();
    assert_eq!(answered.status(), reqwest::StatusCode::OK);
    assert_eq!(answered.text().await.unwrap(), "challenge-echo");
    // A failed decide gets no answer, so the sender gets 503. A handler that
    // panics, or returns an error whose `Display` panics, is logged all the
    // same, and the handler goes on answering.
    for body in ["panic", "unprintable"] {
        let failed = handshake(body).await.unwrap();
        assert_eq!(failed.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let answered = handshake("setup").await.unwrap();
        assert_eq!(answered.status(), reqwest::StatusCode::OK);
        assert_eq!(answered.text().await.unwrap(), "challenge-echo");
    }

    // Each endpoint feeds the service's health with Trellis's check for it:
    // the relayed endpoint is degraded and says why, the token one is fine.
    let caller = runtime
        .register_client::<runtime_trellis::apps::runtime_trellis_observer::Request>(
            "webhook health reader",
        )
        .await
        .unwrap();
    let client = trellis_rs::generated::Client::connect_user(caller.connect_options())
        .await
        .unwrap();
    let health = trellis_health_v1::Client::from_generated(client.clone());
    let request = HealthInspectRequest {
        contract_id: HealthInspectRequestContractId(identity.participant_id().into()),
        history_limit: None,
        history_since: None,
        instance_id: None,
        participant_kind: HealthInspectRequestParticipantKind::Service,
        extra: Default::default(),
    };
    let checks = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(inspected) = health.inspect(&request).await {
                let checks = inspected.instances.first().map(|instance| {
                    serde_json::to_value(instance).unwrap()["latestSample"]["checks"].clone()
                });
                if let Some(checks) = checks.filter(|checks| {
                    checks.as_array().is_some_and(|checks| {
                        checks
                            .iter()
                            .any(|check| check["name"] == "webhook:relayed")
                    })
                }) {
                    break checks;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a heartbeat with the webhook checks");
    let check = |name: &str| {
        checks
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} is reported"))
    };
    assert_eq!(check("webhook:relayed")["status"], "degraded");
    assert_eq!(
        check("webhook:relayed")["summary"],
        "relaySigning not set, so requests are refused"
    );
    assert_eq!(check("webhook:token")["status"], "ok");
    drop(health);
    drop(client);
    runtime.shutdown().await.unwrap();
}
