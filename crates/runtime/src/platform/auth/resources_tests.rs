//! Real store and broker resource materialization; registered under phase_two_tests.

use super::*;

#[test]
fn builtin_provider_replay_preserves_independent_roles_and_refuses_disabled_or_retired_identity() {
    use trellis_runtime_apis::participants::{
        trellis_events_runtime, trellis_health_runtime, trellis_jobs_runtime, trellis_platform,
    };
    let f = fixture();
    let seeds: Vec<_> = (51..55)
        .map(|byte| URL_SAFE_NO_PAD.encode([byte; 32]))
        .collect();
    let platform = f
        .store
        .initialize_builtin_provider::<trellis_platform::Participant>(&seeds[0], NOW)
        .unwrap();
    let health = f
        .store
        .initialize_builtin_provider::<trellis_health_runtime::Participant>(&seeds[1], NOW)
        .unwrap();
    let jobs = f
        .store
        .initialize_builtin_provider::<trellis_jobs_runtime::Participant>(&seeds[2], NOW)
        .unwrap();
    let events = f
        .store
        .initialize_builtin_provider::<trellis_events_runtime::Participant>(&seeds[3], NOW)
        .unwrap();
    let deployments: std::collections::BTreeSet<_> = [&platform, &health, &jobs, &events]
        .map(|provider| provider.deployment_id.clone())
        .into_iter()
        .collect();
    assert_eq!(
        deployments.len(),
        4,
        "roles must not select the same deployment"
    );
    assert!(matches!(
        f.store
            .initialize_builtin_provider::<trellis_health_runtime::Participant>(&seeds[0], NOW),
        Err(AuthError::Denied)
    ));
    assert!(matches!(
        f.store
            .initialize_builtin_provider::<trellis_health_runtime::Participant>(
                &URL_SAFE_NO_PAD.encode([61; 32]),
                NOW
            ),
        Err(AuthError::Conflict)
    ));
    let disabled = PolicyMutation::PrincipalState {
        principal_id: health.principal_id.clone(),
        state: "disabled".into(),
        expected_revision: 1,
    };
    f.store
        .apply_policy(&mutation(&f.admin, &disabled), &disabled, NOW + 1)
        .unwrap();
    let retired = EnforcementScope::Identity(events.identity_key_id.clone());
    f.store
        .revoke_root(&mutation(&f.admin, &retired), &retired, NOW + 1)
        .unwrap();
    let reopened = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let platform_replay = reopened
        .initialize_builtin_provider::<trellis_platform::Participant>(&seeds[0], NOW + 2)
        .unwrap();
    let jobs_replay = reopened
        .initialize_builtin_provider::<trellis_jobs_runtime::Participant>(&seeds[2], NOW + 2)
        .unwrap();
    assert_eq!(platform_replay.deployment_id, platform.deployment_id);
    assert_eq!(platform_replay.instance_id, platform.instance_id);
    assert_eq!(jobs_replay.deployment_id, jobs.deployment_id);
    assert_eq!(jobs_replay.identity_key_id, jobs.identity_key_id);
    assert!(matches!(
        reopened
            .initialize_builtin_provider::<trellis_health_runtime::Participant>(&seeds[1], NOW + 2),
        Err(AuthError::Denied)
    ));
    assert!(matches!(
        reopened
            .initialize_builtin_provider::<trellis_events_runtime::Participant>(&seeds[3], NOW + 2),
        Err(AuthError::Denied)
    ));
}

#[tokio::test]
async fn native_kv_materialization_preserves_values_on_replay_and_rejects_foreign_ownership() {
    use std::{sync::Arc, time::Duration};
    use tokio::io::AsyncBufReadExt;
    let f = fixture();
    let seed = URL_SAFE_NO_PAD.encode([41; 32]);
    let deployment = f.store.initialize_builtin_provider::<trellis_runtime_apis::participants::trellis_platform::Participant>(&seed, NOW).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let binary =
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
    let mut server = tokio::process::Command::new(binary)
        .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
        .arg(directory.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("producer-resolved NATS executable");
    let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if line.contains("Server is ready") {
                return;
            }
        }
        panic!("NATS exited before readiness");
    })
    .await
    .expect("NATS readiness deadline");
    let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    let store = Arc::new(f.store);
    // No declarations really means no resources; do not invent a shared bucket
    // or skip the same installed-declaration materialization path.
    let health_seed = URL_SAFE_NO_PAD.encode([42; 32]);
    let health = store.initialize_builtin_provider::<trellis_runtime_apis::participants::trellis_health_runtime::Participant>(&health_seed,NOW).unwrap();
    let health_bindings =
        super::super::resources::materialize_native_kv(&store, &nats, &health.deployment_id, NOW)
            .await
            .unwrap();
    let mut admission = f.admission.clone();
    admission.credential = Credential::ProvisionedIdentity(health.identity_key_id.clone());
    admission.refresh_family_id = None;
    let issued = store.issue_authority(&admission, None, NOW).unwrap();
    assert_eq!(
        issued.authority.unsigned.binding,
        trellis_protocol::SessionBinding::Service {
            deployment_id: health.deployment_id,
            instance_id: health.instance_id,
            participant_id: health.participant_id
        }
    );
    assert!(health_bindings.kv.is_empty());
    let binding = super::super::resources::materialize_native_kv(
        &store,
        &nats,
        &deployment.deployment_id,
        NOW,
    )
    .await
    .unwrap();
    let jetstream = async_nats::jetstream::new(nats.clone());
    let bucket = jetstream
        .get_key_value(&binding.kv["connections"].bucket)
        .await
        .unwrap();
    // The declaration's desired 16KiB capacity must not become a hard limit.
    let payload = vec![7; 20 * 1024];
    bucket
        .put("presence", payload.clone().into())
        .await
        .unwrap();
    let replay = super::super::resources::materialize_native_kv(
        &store,
        &nats,
        &deployment.deployment_id,
        NOW + 1,
    )
    .await
    .unwrap();
    assert_eq!(
        jetstream
            .get_key_value(&replay.kv["connections"].bucket)
            .await
            .unwrap()
            .get("presence")
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        payload
    );
    assert_eq!(
        bucket.status().await.unwrap().max_age(),
        Duration::from_secs(120)
    );
    let mut stream = jetstream
        .get_stream(format!("KV_{}", binding.kv["connections"].bucket))
        .await
        .unwrap();
    let mut config = stream.info().await.unwrap().config.clone();
    config.description = Some("foreign allocation".into());
    jetstream.update_stream(config).await.unwrap();
    assert!(matches!(
        super::super::resources::materialize_native_kv(
            &store,
            &nats,
            &deployment.deployment_id,
            NOW + 2
        )
        .await,
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        bucket.get("presence").await.unwrap().unwrap().as_ref(),
        payload
    );
    server.kill().await.unwrap();
}

#[test]
fn typed_status_reports_committed_retirement_and_keeps_issued_authority() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let session = issued
        .authority
        .unsigned
        .authorization_session_id
        .parse()
        .unwrap();
    let before = f.store.authorization_session_status(session).unwrap();
    assert_eq!(
        before.state,
        trellis_runtime_apis::types::AuthAuthorizationSessionState::Active
    );
    let scope = super::super::EnforcementScope::Grant(f.grant.clone());
    f.store
        .revoke_root(&super::mutation(&f.admin, &scope), &scope, NOW + 1)
        .unwrap();
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    let status = f
        .store
        .authorization_session_status(
            issued
                .authority
                .unsigned
                .authorization_session_id
                .parse()
                .unwrap(),
        )
        .unwrap();
    let evidence = status.revocation.unwrap();
    let cutoff: trellis_protocol::SignedSessionRevocation =
        serde_json::from_slice(&evidence.0 .0).unwrap();
    assert_eq!(cutoff.effective_cutoff, NOW + 1);
    assert_eq!(
        status.state,
        trellis_runtime_apis::types::AuthAuthorizationSessionState::Revoked
    );
    assert_eq!(
        f.store.resolve_authority(&issued.digest).unwrap(),
        issued.authority
    );
}
