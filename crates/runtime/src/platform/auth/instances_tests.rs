//! Enrollment, its limits, offline retirement, and retention cleanup, against
//! the real Auth store.

use super::*;
use crate::platform::auth::tests::{fixture, mutation, Fixture, NOW};
use ed25519_dalek::SigningKey;
use trellis_protocol::{sign_session_proof, ServiceIdentitySessionProofInput, SessionProofInput};

const ORIGIN: &str = "https://auth.example";

struct Deployment {
    id: String,
    participant: String,
    digest: String,
    provisioned_instance: String,
}

fn service(f: &Fixture) -> Deployment {
    let evidence: trellis_idl::PackageEvidence = f
        .store
        .transaction(|connection| {
            let value: String = connection.query_row(
                "SELECT definition_json FROM auth_accepted_apis WHERE api_id=(SELECT api_id \
                FROM auth_capability_identities WHERE capability_id=?1)",
                [&f.read],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str(&value)?)
        })
        .unwrap();
    let graph = trellis_idl::compile_evidence(evidence.clone()).unwrap();
    let participant = graph
        .root_package()
        .participants()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let input = crate::platform::auth::DeploymentProvisioning {
        evidence,
        participant_id: participant.clone(),
        expected_api_generations: Default::default(),
        identity_public_key: public(&SigningKey::from_bytes(&[200; 32])),
        resource_commitments: json!({}),
        expires_at: None,
    };
    let provisioned = f
        .store
        .provision_deployment(&mutation(&f.admin, &input), &input, NOW)
        .unwrap();
    let digest = f
        .store
        .read(|connection| {
            super::super::rpc_deployments::read_instance(connection, &provisioned.instance_id)
        })
        .unwrap()
        .participant_digest
        .unwrap()
        .to_string();
    Deployment {
        id: provisioned.deployment_id,
        participant,
        digest,
        provisioned_instance: provisioned.instance_id,
    }
}

fn public(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes())
}

fn key_id(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(key.verifying_key().as_bytes()))
}

fn issue(
    f: &Fixture,
    deployment: &Deployment,
    max: u32,
    rate: u32,
    expires: Option<i64>,
) -> (String, String) {
    let input = AuthEnrollmentSecretIssueRequest {
        deployment_id: deployment.id.parse().unwrap(),
        title: String::from("orders, test cluster").into(),
        expires_at: expires.map(|at| timestamp(at).unwrap()),
        max_instances: Some(AuthEnrollmentLimit(max)),
        enrollments_per_hour: Some(AuthEnrollmentLimit(rate)),
        current_build_only: None,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    let issued = issue_secret(&f.store, &mutation(&f.admin, &input), &input, NOW).unwrap();
    (issued.secret.secret_id.to_string(), issued.text.unwrap())
}

fn signed(mut request: Value, key: &SigningKey, status: bool) -> Value {
    let input = ServiceIdentitySessionProofInput {
        origin: ORIGIN.into(),
        unsigned_request: request.clone(),
    };
    let input = if status {
        SessionProofInput::service_identity_status(input)
    } else {
        SessionProofInput::service_enrollment(input)
    }
    .unwrap();
    request["proof"] = serde_json::to_value(sign_session_proof(&input, key).unwrap()).unwrap();
    request
}

fn enrollment(
    secret: &str,
    key: &SigningKey,
    deployment: &Deployment,
    participant: &str,
    now: i64,
) -> Value {
    signed(
        json!({
            "requestId": id(),
            "iat": now * 1000,
            "enrollmentSecret": secret,
            "identityKeyId": key_id(key),
            "identityPublicKey": public(key),
            "participantId": participant,
            "participantDigest": deployment.digest,
            "title": "pod",
        }),
        key,
        false,
    )
}

fn enroll_as(
    f: &Fixture,
    secret: &str,
    key: &SigningKey,
    deployment: &Deployment,
    now: i64,
) -> Result<String, String> {
    match enroll(
        &f.store,
        enrollment(secret, key, deployment, &deployment.participant, now),
        now,
    )
    .unwrap()
    {
        Ok(response) => Ok(response.instance_id.to_string()),
        Err(Refusal::Refused(reason)) => Err(reason.into()),
        Err(other) => Err(format!("{other:?}")),
    }
}

fn status(f: &Fixture, key: &SigningKey, now: i64) -> AuthServiceIdentityState {
    let request = signed(
        json!({
            "requestId": id(),
            "iat": now * 1000,
            "identityKeyId": key_id(key),
            "identityPublicKey": public(key),
        }),
        key,
        true,
    );
    identity_status(&f.store, request, now).unwrap().unwrap()
}

fn instance_state(f: &Fixture, instance: &str) -> (String, Option<String>) {
    f.store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT state,retired_reason FROM auth_instances WHERE instance_id=?1",
                [instance],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap()
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

#[test]
fn a_lost_response_retry_recovers_its_instance_at_the_cap_and_the_rate_limit() {
    let f = fixture();
    let deployment = service(&f);
    let (_, secret) = issue(&f, &deployment, 1, 1, None);
    let first = enroll_as(&f, &secret, &key(1), &deployment, NOW).unwrap();
    // The only slot and the hour's only enrollment are both used, yet the same
    // key gets the same instance back.
    assert_eq!(
        enroll_as(&f, &secret, &key(1), &deployment, NOW + 5),
        Ok(first.clone())
    );
    assert_eq!(
        enroll_as(&f, &secret, &key(2), &deployment, NOW + 5),
        Err("rate_limited".into())
    );
    assert_eq!(
        status(&f, &key(1), NOW + 5),
        AuthServiceIdentityState::Active
    );
}

#[test]
fn a_pod_enrolled_within_the_grace_period_is_not_replaced_until_it_could_have_connected() {
    let f = fixture();
    let deployment = service(&f);
    let (_, secret) = issue(&f, &deployment, 1, 100, None);
    let first = enroll_as(&f, &secret, &key(1), &deployment, NOW).unwrap();
    // A second pod of the same rollout, before the first has connected.
    assert_eq!(
        enroll_as(&f, &secret, &key(2), &deployment, NOW + 60),
        Err("instance_limit".into())
    );
    assert_eq!(instance_state(&f, &first).0, "active");
    // Past the grace period the never-connected one is the longest offline.
    let second = enroll_as(&f, &secret, &key(2), &deployment, NOW + 601).unwrap();
    assert_eq!(
        instance_state(&f, &first),
        ("retired".into(), Some("replaced".into()))
    );
    assert_eq!(instance_state(&f, &second).0, "active");
    assert_eq!(
        status(&f, &key(1), NOW + 602),
        AuthServiceIdentityState::Retired
    );
    // A retired key cannot be enrolled again.
    assert!(enroll_as(&f, &secret, &key(1), &deployment, NOW + 603)
        .unwrap_err()
        .contains("KeyInUse"));
}

#[test]
fn concurrent_enrollments_at_the_cap_cannot_both_take_the_last_slot() {
    let f = fixture();
    let deployment = Arc::new(service(&f));
    let (_, secret) = issue(&f, &deployment, 2, 100, None);
    let store = Arc::clone(&f.store);
    let outcomes: Vec<_> = (0..6u8)
        .map(|seed| {
            let (store, deployment, secret) =
                (Arc::clone(&store), Arc::clone(&deployment), secret.clone());
            std::thread::spawn(move || {
                let body = enrollment(
                    &secret,
                    &key(10 + seed),
                    &deployment,
                    &deployment.participant,
                    NOW,
                );
                enroll(&store, body, NOW).unwrap().is_ok()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|enrolled| **enrolled).count(), 2);
    let live: u32 = f
        .store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM auth_instances \
                WHERE enrollment_secret_id IS NOT NULL AND state='active'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(live, 2);
}

#[test]
fn revoked_expired_unknown_and_other_services_secrets_are_refused() {
    let f = fixture();
    let deployment = service(&f);
    let (expiring_id, expiring) = issue(&f, &deployment, 5, 100, Some(NOW + 10));
    assert_eq!(
        enroll_as(&f, &expiring, &key(1), &deployment, NOW + 11),
        Err("expired".into())
    );
    let (revoked_id, revoked) = issue(&f, &deployment, 5, 100, None);
    let revoke = AuthEnrollmentSecretRevokeRequest {
        secret_id: revoked_id.parse().unwrap(),
        retire_instances: None,
        expected_revision: AuthRevision(1.into()),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    revoke_secret(&f.store, &mutation(&f.admin, &revoke), &revoke, NOW).unwrap();
    assert_eq!(
        enroll_as(&f, &revoked, &key(2), &deployment, NOW + 1),
        Err("revoked".into())
    );
    // A process of another service presenting this deployment's secret.
    let (other_id, other) = issue(&f, &deployment, 5, 100, None);
    assert!(matches!(
        enroll(
            &f.store,
            enrollment(&other, &key(3), &deployment, "acme.billing@v1:billing", NOW),
            NOW,
        )
        .unwrap(),
        Err(Refusal::Refused("participant_mismatch"))
    ));
    // A guessed secret learns nothing and is not audited.
    let guess = format!("{}{}-{}", SECRET_PREFIX, revoked_id, "x".repeat(43));
    assert!(matches!(
        enroll(
            &f.store,
            enrollment(&guess, &key(4), &deployment, &deployment.participant, NOW),
            NOW
        )
        .unwrap(),
        Err(Refusal::Unknown)
    ));
    let alerts = |secret: &str| {
        f.store
            .read(|connection| read_secret(connection, secret, NOW + 20))
            .unwrap()
            .alerts
            .into_iter()
            .map(|alert| (alert.kind, *alert.count))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        alerts(&expiring_id),
        vec![(AuthEnrollmentAlertKind::ExpiredUsed, 1)]
    );
    assert_eq!(
        alerts(&revoked_id),
        vec![(AuthEnrollmentAlertKind::RevokedUsed, 1)]
    );
    assert_eq!(
        alerts(&other_id),
        vec![(AuthEnrollmentAlertKind::ParticipantMismatch, 1)]
    );
    // None of them created anything.
    for seed in 1..=4 {
        assert_eq!(
            status(&f, &key(seed), NOW),
            AuthServiceIdentityState::Unregistered
        );
    }
}

#[test]
fn offline_enrolled_instances_retire_after_the_period_but_not_while_disabled() {
    let f = fixture();
    let deployment = service(&f);
    let (_, secret) = issue(&f, &deployment, 5, 100, None);
    let instance = enroll_as(&f, &secret, &key(1), &deployment, NOW).unwrap();
    let day = 86_400;
    // Within the period, and during the startup quiet period, nothing happens.
    assert_eq!(
        f.store.retire_offline_instances(NOW + day - 1, 0).unwrap(),
        0
    );
    assert_eq!(
        f.store
            .retire_offline_instances(NOW + 2 * day, NOW + 2 * day + 1)
            .unwrap(),
        0
    );
    let change = |state: &str, now: i64| {
        let revision = f
            .store
            .read(|connection| {
                super::super::rpc_deployments::read_deployment(connection, &deployment.id)
            })
            .unwrap()
            .revision;
        let input = AuthDeploymentMutationRequest {
            deployment_id: deployment.id.parse().unwrap(),
            expected_revision: revision,
            idempotency_key: trellis_protocol::model::Ulid::generate().into(),
            extra: Default::default(),
        };
        super::super::rpc_deployments::change_deployment(
            &f.store,
            &mutation(&f.admin, &input),
            &input,
            state,
            now,
        )
        .unwrap();
    };
    change("disabled", NOW + 10);
    assert_eq!(
        f.store.retire_offline_instances(NOW + 3 * day, 0).unwrap(),
        0
    );
    // Enabled again, every instance gets a full period from that moment.
    change("active", NOW + 3 * day);
    assert_eq!(
        f.store
            .retire_offline_instances(NOW + 4 * day - 1, 0)
            .unwrap(),
        0
    );
    assert_eq!(
        f.store.retire_offline_instances(NOW + 4 * day, 0).unwrap(),
        1
    );
    assert_eq!(
        instance_state(&f, &instance),
        ("retired".into(), Some("offline".into()))
    );
    // The hand-provisioned instance stays: retirement covers it only on opt-in.
    assert_eq!(
        instance_state(&f, &deployment.provisioned_instance).0,
        "active"
    );
    let opt_in = AuthInstancePolicySetRequest {
        deployment_id: Some(deployment.id.parse().unwrap()),
        installation: None,
        deployment: Some(AuthDeploymentInstancePolicy {
            retire_offline: None,
            retire_offline_after_seconds: None,
            retire_provisioned: Some(true),
            extra: Default::default(),
        }),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    set_policy(&f.store, &mutation(&f.admin, &opt_in), &opt_in, NOW).unwrap();
    assert_eq!(
        f.store.retire_offline_instances(NOW + 4 * day, 0).unwrap(),
        1
    );
    assert_eq!(
        instance_state(&f, &deployment.provisioned_instance).0,
        "retired"
    );
}

#[test]
fn cleaned_up_instances_keep_tombstones_that_tell_removal_from_retirement() {
    let f = fixture();
    let deployment = service(&f);
    let (_, secret) = issue(&f, &deployment, 5, 100, None);
    let retired = enroll_as(&f, &secret, &key(1), &deployment, NOW).unwrap();
    let removed = enroll_as(&f, &secret, &key(2), &deployment, NOW).unwrap();
    f.store
        .transaction(|connection| retire_in(connection, &retired, "stopped", NOW + 1).map(|_| ()))
        .unwrap();
    let revision = f
        .store
        .read(|connection| super::super::rpc_deployments::read_instance(connection, &removed))
        .unwrap()
        .revision;
    let input = AuthInstanceMutationRequest {
        instance_id: removed.parse().unwrap(),
        expected_revision: revision,
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    super::super::rpc_deployments::change_instance(
        &f.store,
        &mutation(&f.admin, &input),
        &input,
        "service",
        "revoked",
        NOW + 1,
    )
    .unwrap();
    // Kept for 30 days by default.
    assert_eq!(f.store.clean_ended_instances(NOW + 29 * 86_400).unwrap(), 0);
    assert_eq!(f.store.clean_ended_instances(NOW + 31 * 86_400).unwrap(), 2);
    let gone: bool = f
        .store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM auth_instances WHERE instance_id IN (?1,?2)) \
                AND NOT EXISTS(SELECT 1 FROM auth_provisioned_identities \
                    WHERE identity_key_id IN (?3,?4))",
                params![retired, removed, key_id(&key(1)), key_id(&key(2))],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(gone);
    let later = NOW + 31 * 86_400;
    assert_eq!(
        status(&f, &key(1), later),
        AuthServiceIdentityState::Retired
    );
    assert_eq!(
        status(&f, &key(2), later),
        AuthServiceIdentityState::Removed
    );
    assert_eq!(
        status(&f, &key(3), later),
        AuthServiceIdentityState::Unregistered
    );
    // A removed key can never enroll again, even after its rows are gone.
    assert!(enroll_as(&f, &secret, &key(2), &deployment, later)
        .unwrap_err()
        .contains("KeyInUse"));
}
