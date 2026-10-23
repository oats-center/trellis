//! Isolated live Trellis runtimes for Rust integration tests.
//!
//! [`TrellisTestRuntime`] owns production `trellis-server` and NATS processes,
//! acquiring verified release binaries or using explicit exact-version paths.
//! It generates a real bootstrap bundle through the CLI, reserves five loopback ports
//! automatically, seeds a sandbox administrator and completes normal public login,
//! and exposes helpers that accept APIs and provision identities using
//! the generated administration API.
//!
//! Administration uses the same generated production APIs as the runtime.
//!
//! Applications own the generated clients and services they connect; stop those
//! transports before calling [`TrellisTestRuntime::shutdown`].
//!
//! ```no_run
//! # async fn example() -> Result<(), trellis_testkit::TrellisTestError> {
//! use trellis_testkit::TrellisTestRuntime;
//! let mut runtime = TrellisTestRuntime::builder().start().await?;
//! let _url = runtime.trellis_url();
//! runtime.shutdown().await?;
//! # Ok(())
//! # }
//! ```

// The public API returns the structured `TrellisTestError` by value, so the
// large-error lint does not apply to this crate's stable signature.
#![allow(clippy::result_large_err)]

mod admin;
include!("../generated/modules.rs");
mod distribution;
mod error;
mod identity;
mod process;
mod runtime;
mod sandbox;

pub use distribution::NativeSource;
pub use error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
pub use identity::{
    TestClientIdentity, TestRegisteredNativeClient, TestServiceIdentity, TestUserIdentity,
};
pub use runtime::{NatsSource, TestTimeouts, TrellisTestRuntime, TrellisTestRuntimeBuilder};
pub use sandbox::{remove_retained_workdirs, WorkdirRetention};

#[cfg(test)]
mod tests {
    use super::*;
    use trellis_runtime_apis::participants::trellis_health_runtime;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn crashed_ready_server_fails_shutdown_and_retains_failure_diagnostics() {
        let parent = tempfile::tempdir().unwrap();
        let mut runtime = TrellisTestRuntime::builder()
            .workdir_parent(parent.path())
            .retention(WorkdirRetention::OnFailure)
            .start()
            .await
            .unwrap();
        let root = runtime.workdir().to_owned();
        let readiness: serde_json::Value = runtime
            .https_client_builder()
            .build()
            .unwrap()
            .get(format!("{}/readyz", runtime.trellis_url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pid = i32::try_from(readiness["processId"].as_u64().unwrap()).unwrap();
        assert!(pid > 0, "readiness must identify an individual process");
        // SAFETY: readiness identifies this test's owned production server.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                // SAFETY: siginfo_t accepts all-zero initialization, and waitid
                // writes it for our child without consuming its exit status.
                let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe {
                        libc::waitid(
                            libc::P_PID,
                            u32::try_from(pid).unwrap(),
                            &mut status,
                            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                        )
                    },
                    0
                );
                // SAFETY: successful waitid initialized this siginfo_t.
                if unsafe { status.si_pid() } == pid {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let error = runtime.shutdown().await.unwrap_err();
        assert_eq!(error.kind(), TrellisTestErrorKind::Runtime);
        assert!(root.is_dir());
        assert!(!std::fs::read(root.join("logs/trellis-server.log"))
            .unwrap()
            .is_empty());
        runtime.shutdown().await.unwrap();
        assert!(root.is_dir());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_restart_retains_owned_installation_and_stopped_runtime_cannot_restart() {
        let parent = tempfile::tempdir().unwrap();
        let mut runtime = TrellisTestRuntime::builder()
            .workdir_parent(parent.path())
            .retention(WorkdirRetention::OnFailure)
            .start()
            .await
            .unwrap();
        let root = runtime.workdir().to_owned();
        let config = root.join("config/config.toml");
        let retained = root.join("config/retained-config.toml");
        std::fs::rename(config, &retained).unwrap();
        let before = std::fs::read(&retained).unwrap();
        assert!(runtime.restart().await.is_err());
        runtime.shutdown().await.unwrap();
        assert!(root.is_dir());
        assert_eq!(std::fs::read(retained).unwrap(), before);
        assert_eq!(
            runtime.restart().await.unwrap_err().kind(),
            TrellisTestErrorKind::RuntimeStopped
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn server_restart_keeps_the_broker_and_committed_accounts() {
        use trellis_runtime_apis::{
            apis::trellis_auth_v1::Client, apps::trellis_cli::Request, types::AuthUserGetRequest,
        };
        let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
        let user = runtime.create_user("Before restart").await.unwrap();
        let monitor = reqwest::Client::new();
        let before: serde_json::Value = monitor
            .get(format!("{}/varz", runtime.monitor_url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        runtime.restart().await.unwrap();
        let after: serde_json::Value = monitor
            .get(format!("{}/varz", runtime.monitor_url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let broker_id = before["server_id"].as_str().expect("live NATS identity");
        assert!(!broker_id.is_empty());
        assert_eq!(Some(broker_id), after["server_id"].as_str());
        let identity = runtime
            .register_client::<Request>("restart account observer")
            .await
            .unwrap();
        let generated = trellis_rs::generated::Client::connect_user(identity.connect_options())
            .await
            .unwrap();
        let auth = Client::from_generated(generated.clone());
        let retained = auth
            .users_get(&AuthUserGetRequest {
                user_id: user.principal_id().parse().unwrap(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(retained.user.principal_id.to_string(), user.principal_id());
        assert_eq!(retained.user.display_name, "Before restart");
        drop(auth);
        drop(generated);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn admitted_unprivileged_native_caller_reads_issuer_signed_metadata() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use futures_util::StreamExt as _;
        use trellis_protocol::{
            verify_issuer_metadata, IssuerMetadataQuery, IssuerMetadataRequest,
            IssuerMetadataResult, ISSUER_METADATA_SUBJECT,
        };
        let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
        let identity = runtime
            .register_client::<trellis_runtime_apis::apps::trellis_cli::Request>("metadata caller")
            .await
            .unwrap();
        let trust = identity.session.trust.clone();
        let client = trellis_rs::client::TrellisClient::connect_user(identity.connect_options())
            .await
            .unwrap();
        let context = client.authorization_context().unwrap().unwrap().context;
        assert!(context.unsigned.platform_privileges.is_empty());
        let nats = client.runtime_nats();
        let digest = context.digest().unwrap();
        for query in [
            IssuerMetadataQuery::AuthorityResolve {
                context_digest: digest,
            },
            IssuerMetadataQuery::AuthorityStatus {
                authorization_session_id: context.unsigned.authorization_session_id.clone(),
            },
            IssuerMetadataQuery::IssuerChain {
                pinned_key_id: trust.issuer.key_id.clone(),
                target_key_id: trust.issuer.key_id.clone(),
            },
        ] {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let request = IssuerMetadataRequest {
                trellis_instance_id: trust.instance_id.clone(),
                audience_nats_account: trust.account.clone(),
                request_nonce: URL_SAFE_NO_PAD.encode(nonce),
                query,
            };
            let reply = nats.new_inbox();
            let mut replies = nats.subscribe(reply.clone()).await.unwrap();
            nats.flush().await.unwrap();
            nats.publish_with_reply(
                ISSUER_METADATA_SUBJECT,
                reply.clone(),
                serde_json::to_vec(&request).unwrap().into(),
            )
            .await
            .unwrap();
            let message = tokio::time::timeout(std::time::Duration::from_secs(10), replies.next())
                .await
                .unwrap()
                .unwrap();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let verified = verify_issuer_metadata(
                &message.payload,
                &trust.issuer,
                &request,
                ISSUER_METADATA_SUBJECT,
                &reply,
                now,
            )
            .unwrap();
            let IssuerMetadataResult::Value { payload } = verified.result() else {
                panic!("real admitted caller must resolve public verification material")
            };
            match &request.query {
                IssuerMetadataQuery::IssuerKey { .. } => {
                    unreachable!("this caller exercises context, session and rotation reads")
                }
                IssuerMetadataQuery::AuthorityResolve { .. } => {
                    let resolved: trellis_runtime_apis::types::AuthAuthorityResolveResponse =
                        serde_json::from_str(payload).unwrap();
                    assert_eq!(
                        trellis_protocol::parse_session_authority(&resolved.authority.0 .0, 65536)
                            .unwrap(),
                        context
                    );
                }
                IssuerMetadataQuery::AuthorityStatus { .. } => {
                    let resolved: trellis_runtime_apis::types::AuthAuthorityStatusResponse =
                        serde_json::from_str(payload).unwrap();
                    assert_eq!(
                        resolved.authorization_session_id.to_string(),
                        context.unsigned.authorization_session_id
                    );
                    assert_eq!(
                        resolved.state,
                        trellis_runtime_apis::types::AuthAuthorizationSessionState::Active
                    );
                }
                IssuerMetadataQuery::IssuerChain { .. } => {
                    let resolved: trellis_runtime_apis::types::AuthIssuerChainResponse =
                        serde_json::from_str(payload).unwrap();
                    assert!(resolved.rotations.is_empty());
                }
            }
            let mut substituted = request.clone();
            substituted.request_nonce = URL_SAFE_NO_PAD.encode([91; 16]);
            assert!(verify_issuer_metadata(
                &message.payload,
                &trust.issuer,
                &substituted,
                ISSUER_METADATA_SUBJECT,
                &reply,
                now
            )
            .is_err());
            assert!(verify_issuer_metadata(
                &message.payload,
                &trust.issuer,
                &request,
                ISSUER_METADATA_SUBJECT,
                &reply,
                now + 31
            )
            .is_err());
            let mut forged: trellis_protocol::SignedIssuerMetadata =
                serde_json::from_slice(&message.payload).unwrap();
            forged.response.result = IssuerMetadataResult::Missing;
            assert!(verify_issuer_metadata(
                &serde_json::to_vec(&forged).unwrap(),
                &trust.issuer,
                &request,
                ISSUER_METADATA_SUBJECT,
                &reply,
                now
            )
            .is_err());
        }
        drop(nats);
        drop(client);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn signed_native_heartbeat_projects_provider_identity() {
        use trellis_runtime_apis::{apis::trellis_health_v1, types::*};
        let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
        let identity = runtime
            .register_service::<trellis_health_runtime::Participant>("health acceptance provider")
            .await
            .unwrap();
        let publication_started_at = time::OffsetDateTime::now_utc();
        let provider = trellis_health_runtime::Participant::connect(identity.connect_options())
            .await
            .unwrap();
        let caller = runtime
            .register_client::<trellis_runtime_apis::apps::trellis_cli::Request>("health observer")
            .await
            .unwrap();
        let client = trellis_rs::generated::Client::connect_user(caller.connect_options())
            .await
            .unwrap();
        let health = trellis_health_v1::Client::from_generated(client.clone());
        let request = HealthQueryRequest {
            contract_ids: Some(vec![HealthQueryRequestContractIdsItem(
                identity.participant_id().into(),
            )]),
            deployment_ids: Some(vec![HealthQueryRequestDeploymentIdsItem(
                identity.deployment_id().into(),
            )]),
            participant_kinds: Some(vec![HealthQueryRequestParticipantKinds::Service]),
            page: None,
            search: None,
            statuses: None,
            extra: Default::default(),
        };
        let projected = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                match health.query(&request).await {
                    Ok(result) if !result.items.is_empty() => break result,
                    Ok(_) => {}
                    Err(error) => panic!("health query failed: {error}"),
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("native heartbeat must reach the SQL projection through historical verification");
        let sample = &projected.items[0];
        assert_eq!(sample.contract_id, identity.participant_id());
        assert_eq!(
            sample
                .deployment_ids
                .iter()
                .map(AsRef::<str>::as_ref)
                .collect::<Vec<_>>(),
            vec![identity.deployment_id()]
        );
        assert_eq!(sample.participant_name, identity.name());
        assert_eq!(sample.online_instances.0, 1);
        assert_eq!(
            sample.effective_status,
            HealthQueryResponseentriesItemEffectiveStatus::Healthy
        );
        let observed_at = time::OffsetDateTime::parse(
            &sample.last_seen_at,
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert!(
            observed_at >= publication_started_at && observed_at <= time::OffsetDateTime::now_utc()
        );
        drop(provider);
        drop(health);
        drop(client);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_caller_uses_real_portal_consent_and_certified_account_rpc() {
        let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
        let identity = runtime
            .register_client::<trellis_runtime_apis::apps::trellis_cli::Request>("native caller")
            .await
            .unwrap();
        let client = trellis_rs::generated::Client::connect_user(identity.connect_options())
            .await
            .unwrap();
        let auth =
            trellis_runtime_apis::apis::trellis_auth_v1::Client::from_generated(client.clone());
        let account = auth
            .users_resolve(&trellis_runtime_apis::types::AuthEmpty {
                extra: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            account.user.account.as_ref().unwrap().username,
            runtime.admin_username()
        );
        let directory = auth
            .users_get(&trellis_runtime_apis::types::AuthUserGetRequest {
                user_id: account.user.principal_id,
                extra: Default::default(),
            })
            .await
            .unwrap();
        // Directory consent does not delegate the shared administrator's
        // account-administration privilege to this application.
        assert!(directory.user.account.is_none());
        let missing = auth
            .users_get(&trellis_runtime_apis::types::AuthUserGetRequest {
                user_id: ulid::Ulid::new().to_string().parse().unwrap(),
                extra: Default::default(),
            })
            .await
            .unwrap_err();
        let trellis_rs::client::CallError::Declared(error) = missing else {
            panic!(
                "unprivileged native registration must return a verified declared error, \
                not a transport failure"
            )
        };
        let trellis_runtime_apis::apis::trellis_auth_v1::rpc::UsersGetError::AuthError(error) =
            error.as_ref()
        else {
            panic!("unprivileged native registration must return the Auth error contract")
        };
        assert_eq!(error.payload().unwrap().code.as_ref(), "not_found");
        drop(auth);
        drop(client);
        runtime.shutdown().await.unwrap();
    }
}
