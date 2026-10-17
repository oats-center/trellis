use async_nats::HeaderMap;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use trellis_rs::auth::{NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::{
    ResponseVerificationInput, ServiceConnectWithContractOptions, TrellisClient, UserConnectOptions,
};
use trellis_rs::generated::RpcDescriptor;
use trellis_rs::service::internal::{
    dispatch_one, encode_success_reply, sign_verified_reply, InboundRequest,
};
use trellis_rs::service::{
    AuthenticatedRouter, DeclaredRpcError, LiveProviderOwner, RequestContext, Router, ServerError,
};
use trellis_runtime_apis::apis::{trellis_auth_v1, trellis_health_v1::rpc::Query};
use trellis_runtime_apis::participants::trellis_health_runtime::Participant;
use trellis_runtime_apis::types::{AuthInstanceMutationRequest, AuthLoginRevokeRequest};

// Real OAuth retirement after the production router admits finite work must not
// suppress its authentic result. This does not promise delivery to a kicked peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_finite_replies_survive_caller_retirement() {
    let mut runtime = trellis_testkit::TrellisTestRuntime::builder()
        .start()
        .await
        .unwrap();
    let identity = runtime
        .register_service::<Participant>("finite-reply-provider")
        .await
        .unwrap();
    let server = trellis_rs::auth::discover_native_oauth(runtime.oauth_origin())
        .await
        .unwrap();
    let provider = Arc::new(
        TrellisClient::connect_service_with_contract(ServiceConnectWithContractOptions {
            nats_servers: server.nats_servers.clone(),
            trust: server.trust.clone(),
            participant_id: identity.participant_id(),
            provisioned_identity_seed_base64url: identity.seed(),
            name: Some(identity.name()),
            timeout_ms: 10_000,
        })
        .await
        .unwrap(),
    );
    let provider_runtime = LiveProviderOwner::from_connected_client(provider.clone());
    let provider_owner = provider_runtime.runtime_authorization_owner();
    let provider_nats = provider_runtime.runtime_nats();
    let native = NativeOAuthClient::open(
        runtime
            .workdir()
            .join("credentials/finite-reply-controller"),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust,
    )
    .await
    .unwrap();
    let mut controller_capabilities = vec![
        trellis_auth_v1::rpc::SessionsRevoke::CALLER_CAPABILITIES[0],
        trellis_auth_v1::rpc::ServiceInstancesRemove::CALLER_CAPABILITIES[0],
    ];
    controller_capabilities.sort_unstable();
    controller_capabilities.dedup();
    let login = native
        .start_device(&json!([
            {"type":"trellis_capabilities","required":controller_capabilities,"optional":[]},
            {"type":"trellis_platform_privileges","required":["principals.manage"]}
        ]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", login.user_code())
        .await
        .unwrap();
    let controller = trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
        server.nats_servers,
        10_000,
        login.complete().await.unwrap(),
    ))
    .await
    .unwrap();
    let auth = trellis_auth_v1::Client::from_generated(controller.clone());
    let action = trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: "Query".into(),
        direction: trellis_protocol::PermissionAction::Call,
    };
    let mut messages = provider_nats.subscribe(Query::SUBJECT).await.unwrap();
    provider_nats.flush().await.unwrap();
    let mut retained = None;
    for is_error in [false, true] {
        let caller_identity = runtime
            .register_client::<trellis_runtime_apis::apps::trellis_cli::Request>(if is_error {
                "finite-error-caller"
            } else {
                "finite-success-caller"
            })
            .await
            .unwrap();
        let caller = Arc::new(
            TrellisClient::connect_user(caller_identity.connect_options())
                .await
                .unwrap(),
        );
        let caller_runtime = LiveProviderOwner::from_connected_client(caller.clone());
        let caller_owner = caller_runtime.runtime_authorization_owner();
        let caller_nats = caller_runtime.runtime_nats();
        let own = caller_owner.own_authority().unwrap();
        let api = own
            .authority()
            .apis
            .iter()
            .find(|api| api.api_id == Query::API_ID)
            .unwrap();
        let opening = trellis_protocol::SessionRequest {
            authority_digest: own.digest().into(),
            api_id: Query::API_ID.into(),
            api_generation: api.generation,
            accepted_revision: api.accepted_revision,
            action: action.name.clone(),
            subject: Query::SUBJECT.into(),
            reply_subject: Some(caller_nats.new_inbox()),
            request_id: ulid::Ulid::new().to_string(),
            issued_at: caller_owner.policy().now_unix_seconds,
        };
        let payload = Bytes::from_static(b"{}");
        let (proof, digest) = caller
            .auth()
            .create_request_proof(&opening, &payload)
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "Trellis-Session-Request",
            serde_json::to_string(&opening).unwrap(),
        );
        headers.insert("proof", proof.as_str());
        caller_nats
            .publish_with_reply_and_headers(
                Query::SUBJECT,
                opening.reply_subject.clone().unwrap(),
                headers,
                payload.clone(),
            )
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(10), messages.next())
            .await
            .unwrap()
            .unwrap();
        let mut context = RequestContext::default();
        context.subject = message.subject.to_string();
        context.reply_to = message.reply.as_ref().map(ToString::to_string);
        context.session_request = Some(
            serde_json::from_str(
                message
                    .headers
                    .as_ref()
                    .unwrap()
                    .get("Trellis-Session-Request")
                    .unwrap()
                    .as_str(),
            )
            .unwrap(),
        );
        context.proof = Some(
            message
                .headers
                .as_ref()
                .unwrap()
                .get("proof")
                .unwrap()
                .as_str()
                .into(),
        );
        let request = InboundRequest {
            subject: message.subject.to_string(),
            payload: message.payload,
            reply_to: context.reply_to.clone(),
            context: context.clone(),
        };
        let admitted = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut router = Router::new();
        router.register_rpc::<Query, _, _>({
            let admitted = admitted.clone();
            let release = release.clone();
            move |_, _| {
                let admitted = admitted.clone();
                let release = release.clone();
                async move {
                    admitted.notify_one();
                    release.notified().await;
                    if is_error {
                        Err(ServerError::DeclaredRpc(DeclaredRpcError::new(
                            "trellis.health@v1::ValidationError",
                            "Delayed business failure",
                            [("issues", json!([]))],
                        )))
                    } else {
                        Ok(serde_json::from_value(json!({
                            "items":[], "page":{"nextCursor":"delayed-result"}
                        }))
                        .unwrap())
                    }
                }
            }
        });
        let handler = Arc::new(AuthenticatedRouter::new(
            router,
            provider_runtime.runtime_validator(Query::API_ID),
        ));
        // Invalid opening bytes cannot create a signing-capable verified context.
        let mut invalid_context = RequestContext::default();
        invalid_context.subject = context.subject.clone();
        invalid_context.reply_to = context.reply_to.clone();
        invalid_context.session_request = context.session_request.clone();
        invalid_context.proof = context.proof.clone();
        assert!(matches!(
            handler
                .handle_request(
                    Query::SUBJECT,
                    Bytes::from_static(b"{\"search\":\"altered\"}"),
                    invalid_context.clone()
                )
                .await,
            Err(ServerError::RequestDenied { .. })
        ));
        assert!(sign_verified_reply(
            &provider,
            &invalid_context,
            &action,
            &encode_success_reply(opening.reply_subject.clone().unwrap(), payload.clone())
        )
        .await
        .is_err());
        let task = tokio::spawn({
            let handler = handler.clone();
            async move {
                dispatch_one(handler.as_ref(), request)
                    .await
                    .unwrap()
                    .unwrap()
            }
        });
        tokio::time::timeout(Duration::from_secs(10), admitted.notified())
            .await
            .unwrap();
        auth.sessions_revoke(&AuthLoginRevokeRequest {
            login_session_id: own
                .authority()
                .login_session_id
                .as_ref()
                .unwrap()
                .parse()
                .unwrap(),
            idempotency_key: ulid::Ulid::new().to_string().into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while provider_owner
                .assert_current(context.verified_request().unwrap().authority())
                .is_ok()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // A new, genuinely signed opening under the retired caller is denied.
        let fresh = trellis_protocol::SessionRequest {
            request_id: ulid::Ulid::new().to_string(),
            ..opening.clone()
        };
        let (fresh_proof, _) = caller
            .auth()
            .create_request_proof(&fresh, &payload)
            .unwrap();
        let mut denied_context = RequestContext::default();
        denied_context.subject = Query::SUBJECT.into();
        denied_context.reply_to = fresh.reply_subject.clone();
        denied_context.session_request = Some(fresh);
        denied_context.proof = Some(fresh_proof.as_str().into());
        assert!(matches!(
            handler
                .handle_request(Query::SUBJECT, payload.clone(), denied_context.clone())
                .await,
            Err(ServerError::RequestDenied { .. })
        ));
        assert!(sign_verified_reply(
            &provider,
            &denied_context,
            &action,
            &encode_success_reply(opening.reply_subject.clone().unwrap(), payload.clone())
        )
        .await
        .is_err());
        release.notify_one();
        let reply = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply.is_error, is_error);
        let signed = sign_verified_reply(&provider, &context, &action, &reply)
            .await
            .unwrap();
        let response: trellis_protocol::SessionResponse =
            serde_json::from_str(signed.get("Trellis-Session-Response").unwrap().as_str()).unwrap();
        let proof =
            trellis_protocol::SessionResponseProof::parse(signed.get("proof").unwrap().as_str())
                .unwrap();
        provider_owner
            .verify_response(
                &provider_nats,
                ResponseVerificationInput {
                    pending_request: &opening,
                    pending_request_digest: &digest,
                    action: &action,
                    expected_deployment_id: Some(identity.deployment_id()),
                    received_subject: &reply.reply_to,
                    response: &response,
                    payload: &reply.payload,
                    proof: &proof,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            response.kind,
            if is_error {
                trellis_protocol::SessionResponseKind::Error
            } else {
                trellis_protocol::SessionResponseKind::Success
            }
        );
        if is_error {
            assert!(matches!(
                Query::decode_error(serde_json::from_slice(&reply.payload).unwrap()).unwrap(),
                Some(
                    trellis_runtime_apis::apis::trellis_health_v1::rpc::QueryError::ValidationError(
                        _
                    )
                )
            ));
        } else {
            let result: trellis_runtime_apis::types::HealthQueryResponse =
                serde_json::from_slice(&reply.payload).unwrap();
            assert_eq!(result.page.next_cursor.as_deref(), Some("delayed-result"));
        }
        assert!(provider_owner
            .verify_response(
                &provider_nats,
                ResponseVerificationInput {
                    pending_request: &opening,
                    pending_request_digest: &digest,
                    action: &action,
                    expected_deployment_id: Some(identity.deployment_id()),
                    received_subject: &reply.reply_to,
                    response: &response,
                    payload: b"tampered",
                    proof: &proof,
                },
            )
            .await
            .is_err());
        retained = Some((context, reply));
        caller_runtime.shutdown_native_runtime().await.unwrap();
    }
    // Retire the real provider through the generated instance lifecycle API,
    // not by fabricating a revoked authority or mutating private storage.
    let authority = provider_owner.own_authority().unwrap();
    auth.service_instances_remove(&AuthInstanceMutationRequest {
        instance_id: identity.instance_id().parse().unwrap(),
        expected_revision: trellis_runtime_apis::types::AuthRevision(1.into()),
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while provider_owner.assert_current(&authority).is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (context, reply) = retained.unwrap();
    assert!(sign_verified_reply(&provider, &context, &action, &reply)
        .await
        .is_err());
    provider_runtime.shutdown_native_runtime().await.unwrap();
    drop(messages);
    drop(auth);
    drop(controller);
    runtime.shutdown().await.unwrap();
}
