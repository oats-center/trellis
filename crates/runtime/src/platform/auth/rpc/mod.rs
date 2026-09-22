//! Exact Auth RPC routing and workflow dispatch.

mod error;
mod router;
mod workflows;

use error::public_rpc_error;

use async_nats::header::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trellis_protocol::AuthorizationPrincipalKind;
use trellis_rs::service::Router;
use trellis_runtime_apis::apis::trellis_auth_v1::events::{
    DeviceUserAuthoritiesApproved, DeviceUserAuthoritiesResolved, SessionsRevoked,
};
use ulid::Ulid;

use super::context::AuthorizationContextRepository;
use super::{
    activation_review_event_action_id, auth_event_subject, AccountFlowKind, AccountRepository,
    AuthConnectionPresence, AuthEphemeralRepository, AuthService, AuthorityEvidenceRepository,
    AuthorizationStateError, CapabilityGroupRecord, ChangePasswordInput, CreateAccountFlowInput,
    CreateUserInput, DecideActivationReviewInput, DeploymentProfileCreation,
    DeploymentProfileMutation, DeploymentProfileRecord, DeploymentProfileState,
    DeploymentRepository, DeviceActivationReviewRecord, DeviceActivationReviewState,
    DeviceDelegationMutation, DeviceDelegationState, DeviceReviewMode, GrantOwnerKind,
    IdempotencyResultRecord, IdempotentOutcome, LoginPortalMutation, LoginPortalRecord,
    LoginSettingsRecord, NatsAuthEphemeralRepository, PortalGrantOverrideRecord,
    PortalPolicyReconciliationHandle, PortalRepository, PortalRoleMapping, PortalRouteMutation,
    PortalRouteRecord, PortalRouteRemoval, PostCommitActionKind, PostCommitActionRecord,
    PrincipalKind, PrincipalState, ProviderIdentityUnlink, ProvisionDeviceInput,
    ProvisionServiceIdentityInput, ProvisionedIdentityKind, ProvisionedIdentityRecord,
    ProvisionedIdentityState, ProvisionedInstanceMutation, ProvisioningRepository,
    ResourceBindingEvidence, RuntimeInstanceState, SessionRecord, SessionRepository, SessionState,
    SqliteAuthorizationStore, UpdateUserInput, UserAccount,
};
use crate::shutdown::StopHandle;
use crate::supervisor::RuntimeError;

const MAX_CONCURRENT_REQUESTS: usize = 64;

pub(crate) struct AuthRpcRuntime {
    subscriber: futures_util::stream::SelectAll<async_nats::Subscriber>,
    processor: AuthRpcProcessor,
    core_subject_prefix: String,
}

#[derive(Clone)]
pub(crate) struct AuthRpcProcessor {
    pub(crate) client: async_nats::Client,
    pub(crate) service: AuthService<SqliteAuthorizationStore>,
    pub(crate) ephemeral: NatsAuthEphemeralRepository,
    pub(crate) public_origin: String,
    pub(crate) verifier: crate::platform::auth::verifier::RuntimeAuthVerifier,
    pub(crate) routes: Arc<Router>,
    pub(crate) portal_reconciliation: PortalPolicyReconciliationHandle,
}

struct ValidatedRequest {
    principal_id: String,
    principal_kind: PrincipalKind,
    context: trellis_protocol::VerifiedAuthorizationContext,
    session_public_key: String,
    platform_privileges: Vec<trellis_protocol::PlatformPrivilege>,
}

fn mutation_actor(caller: &ValidatedRequest) -> super::MutationActor {
    super::MutationActor {
        context_digest: caller.context.context_digest().to_owned(),
        principal_id: caller.principal_id.clone(),
        participant_id: caller.context.participant_id().to_owned(),
        owner_kind: caller.context.owner_kind(),
        owner_id: caller.context.owner_id().to_owned(),
        grant_revision: caller.context.grant_revision(),
        login_session_id: caller.context.login_session_id().map(str::to_owned),
        session_public_key: caller.session_public_key.clone(),
    }
}

impl AuthRpcRuntime {
    pub(crate) async fn start(
        processor: AuthRpcProcessor,
    ) -> Result<Self, AuthorizationStateError> {
        let auth_subject_prefix =
            rpc_subject_prefix(trellis_runtime_apis::apis::trellis_auth_v1::API_ID)?;
        let core_subject_prefix =
            rpc_subject_prefix(trellis_runtime_apis::apis::trellis_core_v1::API_ID)?;
        let auth = processor
            .client
            .queue_subscribe(
                format!("{auth_subject_prefix}>"),
                "trellis-auth-rpc".to_owned(),
            )
            .await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        let resources = processor
            .client
            .queue_subscribe(
                format!("{core_subject_prefix}>"),
                "trellis-resource-rpc".to_owned(),
            )
            .await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        let mut subscriber = futures_util::stream::SelectAll::new();
        subscriber.push(auth);
        subscriber.push(resources);
        Ok(Self {
            subscriber,
            processor,
            core_subject_prefix,
        })
    }

    fn api_domain(&self, subject: &str) -> error::ApiDomain {
        if subject.starts_with(&self.core_subject_prefix) {
            error::ApiDomain::Core
        } else {
            error::ApiDomain::Auth
        }
    }

    pub(crate) async fn run(mut self, stop: StopHandle) -> Result<(), RuntimeError> {
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                () = stop.stopped() => break,
                result = requests.join_next(), if !requests.is_empty() => {
                    if let Some(result) = result {
                        result.map_err(|error| RuntimeError::Platform(error.to_string()))??;
                    }
                }
                message = self.subscriber.next(), if requests.len() < MAX_CONCURRENT_REQUESTS => {
                    let Some(message) = message else {
                        return Err(RuntimeError::Platform("Auth RPC subscription closed".to_owned()));
                    };
                    let processor = self.processor.clone();
                    let domain = self.api_domain(&message.subject);
                    requests.spawn(async move { processor.process(message, domain).await });
                }
            }
        }
        requests.abort_all();
        Ok(())
    }
}

impl AuthRpcProcessor {
    async fn process(
        &self,
        message: async_nats::Message,
        domain: error::ApiDomain,
    ) -> Result<(), RuntimeError> {
        tracing::debug!(subject = %message.subject, reply = ?message.reply, "processing Auth RPC request");
        let Some(reply) = message.reply.clone() else {
            return Ok(());
        };
        let subject = message.subject.as_str();
        let dispatch_started = Instant::now();
        let result = self.dispatch(subject, &message).await;
        let dispatch_elapsed = dispatch_started.elapsed();
        if dispatch_elapsed >= Duration::from_secs(1) {
            tracing::warn!(
                subject,
                duration_ms = dispatch_elapsed.as_millis(),
                "Auth RPC dispatch exceeded one second"
            );
        }
        tracing::debug!(
            subject,
            success = result.is_ok(),
            "finished Auth RPC dispatch"
        );
        let (headers, payload) = match result {
            Ok(mut value) => {
                encode_generated_scalars(&mut value);
                (HeaderMap::new(), serde_json::to_vec(&value))
            }
            Err(error) => {
                tracing::warn!(subject, %error, "Auth RPC request failed");
                let mut headers = HeaderMap::new();
                headers.insert("status", "error");
                let mut error = public_rpc_error(domain, &error);
                encode_generated_scalars(&mut error);
                (headers, serde_json::to_vec(&error))
            }
        };
        let payload = payload.map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let publish_started = Instant::now();
        self.client
            .publish_with_headers(reply, headers, Bytes::from(payload))
            .await
            .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let publish_elapsed = publish_started.elapsed();
        if publish_elapsed >= Duration::from_secs(1) {
            tracing::warn!(
                subject,
                duration_ms = publish_elapsed.as_millis(),
                "Auth RPC reply publish exceeded one second"
            );
        }
        tracing::debug!(subject, "published Auth RPC response");
        Ok(())
    }

    async fn dispatch(
        &self,
        subject: &str,
        message: &async_nats::Message,
    ) -> Result<Value, AuthorizationStateError> {
        router::dispatch(self, subject, message).await
    }

    async fn deployments_create(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let wire: trellis_runtime_apis::apis::trellis_auth_v1::rpc::DeploymentsCreateInput =
            serde_json::from_slice(payload)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let kind = match required_string(&input, "kind")? {
            "service" => PrincipalKind::Service,
            "device" => PrincipalKind::Device,
            _ => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "deployment kind is invalid".to_owned(),
                ));
            }
        };
        let review_mode = match wire.review_mode {
            trellis_runtime_apis::__types::Nullable::Null => None,
            trellis_runtime_apis::__types::Nullable::Value(bytes) => {
                Some(serde_json::from_slice::<String>(&bytes.0).map_err(|_| {
                    AuthorizationStateError::InvalidRecord(
                        "deployment reviewMode bytes are invalid".to_owned(),
                    )
                })?)
            }
        };
        let review_mode = match (kind, review_mode.as_deref()) {
            (PrincipalKind::Device, Some("none")) => Some(DeviceReviewMode::None),
            (PrincipalKind::Device, Some("required")) => Some(DeviceReviewMode::Required),
            (PrincipalKind::Service, None) => None,
            (PrincipalKind::Device, _) => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "device deployment reviewMode must be none or required".to_owned(),
                ));
            }
            (PrincipalKind::Service, _) => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "service deployment reviewMode must be null".to_owned(),
                ));
            }
            (PrincipalKind::User, _) => unreachable!("deployment kind excludes users"),
        };
        let now = now_millis()?;
        let deployment_id = format!("dep_{}", Ulid::new());
        let profile = DeploymentProfileRecord {
            deployment_id: deployment_id.clone(),
            kind,
            display_name: required_string(&input, "displayName")?.to_owned(),
            participant_id: nullable_string(&input, "participantId")?,
            portal_id: nullable_string(&input, "portalId")?,
            review_mode,
            requires_device_delegation: required_bool(&input, "requiresDeviceDelegation")?,
            expires_at: required_nullable_i64(&input, "expiresAt")?,
            state: DeploymentProfileState::Active,
            created_at: now,
            updated_at: now,
            version: 1,
        };
        let mut idempotency = rpc_idempotency(
            "Auth.Deployments.Create",
            &caller.principal_id,
            required_string(&input, "idempotencyKey")?,
            &input,
            now,
        )?;
        let result =
            json!({ "deployment": self.deployment_value_with_times(profile.clone(), None, None) });
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .create_deployment_profile(DeploymentProfileCreation {
                principal: super::PrincipalRecord {
                    principal_id: deployment_id.clone(),
                    kind,
                    state: PrincipalState::Active,
                    created_at: now,
                    updated_at: now,
                    version: 1,
                    disabled_at: None,
                    revoked_at: None,
                },
                profile: profile.clone(),
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        match outcome {
            IdempotentOutcome::Applied(_) => Ok(result),
            IdempotentOutcome::Replayed(replay) => Ok(replay),
        }
    }

    async fn deployments_list(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let kind = input.get("kind").and_then(Value::as_str);
        let state = input.get("state").and_then(Value::as_str);
        let mut entries = Vec::new();
        for profile in self.service.repository().list_deployment_profiles().await? {
            if kind.is_some_and(|value| enum_string(profile.kind) != value)
                || state.is_some_and(|value| deployment_state_wire(profile.state) != value)
            {
                continue;
            }
            entries.push(self.deployment_value(profile).await?);
        }
        paginate_values(entries, &input, "auth.Deployments.List", &["/deploymentId"])
    }

    async fn deployments_get(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        require_admin(caller)?;
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let deployment_id = required_string(&input, "deploymentId")?;
        let profile = self
            .service
            .repository()
            .get_deployment_profile(deployment_id)
            .await?
            .ok_or(AuthorizationStateError::NotFound)?;
        let participant_id = profile.participant_id.clone();
        let binding = match &participant_id {
            Some(participant_id) => {
                self.service
                    .repository()
                    .get_grant_binding(
                        GrantOwnerKind::Deployment,
                        deployment_id.to_owned(),
                        participant_id.clone(),
                    )
                    .await?
            }
            None => None,
        };
        let resources = match (&binding, participant_id) {
            (Some(binding), Some(participant_id)) => self
                .service
                .repository()
                .get_resource_bindings(
                    GrantOwnerKind::Deployment,
                    deployment_id.to_owned(),
                    participant_id,
                    binding.installed_revision,
                )
                .await?
                .into_iter()
                .map(resource_binding_value)
                .collect::<Result<Vec<_>, _>>()?,
            _ => Vec::new(),
        };
        Ok(json!({
            "deployment": self.deployment_value(profile).await?,
            "binding": binding,
            "resources": resources,
        }))
    }

    async fn deployments_set_state(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
        state: DeploymentProfileState,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let deployment_id = required_string(&input, "deploymentId")?;
        let expected_version = required_u64(&input, "expectedVersion")?;
        let mut profile = self
            .service
            .repository()
            .get_deployment_profile(deployment_id)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("deployment not found".to_owned())
            })?;
        let now = now_millis()?;
        profile.state = state;
        profile.updated_at = now;
        profile.version = profile
            .version
            .checked_add(1)
            .ok_or_else(|| AuthorizationStateError::Storage("version overflow".to_owned()))?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let actions = (state != DeploymentProfileState::Active)
            .then(|| PostCommitActionRecord {
                predecessor_action_id: None,
                action_id: digest_parts(&[deployment_id, idempotency_key, "kick"]),
                kind: PostCommitActionKind::Kick,
                payload: json!({ "deploymentId": deployment_id }),
                created_at: now,
                attempts: 0,
                next_attempt_at: now,
                claimed_until: None,
                last_error: None,
            })
            .into_iter()
            .collect();
        let mut idempotency = rpc_idempotency(
            "Auth.Deployments.State",
            &caller.principal_id,
            idempotency_key,
            &input,
            now,
        )?;
        let result = json!({
            "deployment": self.deployment_value_with_times(
                profile.clone(),
                (state == DeploymentProfileState::Disabled).then_some(now),
                (state == DeploymentProfileState::Removed).then_some(now),
            ),
            "mutation": {
                "resourceId": deployment_id,
                "state": deployment_state_wire(state),
                "version": profile.version,
                "changed": true,
            }
        });
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .put_deployment_profile(DeploymentProfileMutation {
                profile: profile.clone(),
                expected_version,
                idempotency,
                actions,
            })
            .await?;
        match outcome {
            IdempotentOutcome::Applied(_) => Ok(result),
            IdempotentOutcome::Replayed(replay) => Ok(replay),
        }
    }

    async fn deployment_value(
        &self,
        profile: DeploymentProfileRecord,
    ) -> Result<Value, AuthorizationStateError> {
        let principal = self
            .service
            .repository()
            .get_principal(&profile.deployment_id)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        Ok(self.deployment_value_with_times(profile, principal.disabled_at, principal.revoked_at))
    }

    fn deployment_value_with_times(
        &self,
        profile: DeploymentProfileRecord,
        disabled_at: Option<i64>,
        revoked_at: Option<i64>,
    ) -> Value {
        let mut value = json!({
            "deploymentId": profile.deployment_id,
            "kind": profile.kind,
            "displayName": profile.display_name,
            "state": deployment_state_wire(profile.state),
            "participantId": profile.participant_id,
            "expiresAt": profile.expires_at.map(|value| value.to_string()),
            "reviewMode": profile.review_mode,
            "requiresDeviceDelegation": profile.requires_device_delegation,
            "portalId": profile.portal_id,
            "createdAt": profile.created_at.to_string(),
            "updatedAt": profile.updated_at.to_string(),
            "disabledAt": disabled_at.map(|value| value.to_string()),
            "revokedAt": revoked_at.map(|value| value.to_string()),
            "version": profile.version.to_string(),
            "disabled": profile.state != DeploymentProfileState::Active,
        });
        if profile.kind == PrincipalKind::Service {
            value["namespaces"] = json!([]);
        }
        value
    }

    async fn bind_deployment_participant(
        &self,
        deployment_id: &str,
        requested_participant_id: Option<String>,
        expected_kind: PrincipalKind,
        input: &Value,
        caller: &ValidatedRequest,
        now: i64,
    ) -> Result<DeploymentProfileRecord, AuthorizationStateError> {
        let mut profile = self
            .service
            .repository()
            .get_deployment_profile(deployment_id)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("deployment not found".to_owned())
            })?;
        if profile.kind != expected_kind || profile.state != DeploymentProfileState::Active {
            return Err(AuthorizationStateError::StorageConflict);
        }
        if profile
            .participant_id
            .as_ref()
            .zip(requested_participant_id.as_ref())
            .is_some_and(|(current, requested)| current != requested)
        {
            return Err(AuthorizationStateError::StorageConflict);
        }
        if profile.participant_id.is_none() {
            profile.participant_id = requested_participant_id;
            if profile.participant_id.is_none() {
                return Err(AuthorizationStateError::InvalidRecord(
                    "participantId is required before provisioning".to_owned(),
                ));
            }
            let expected_version = profile.version;
            profile.version = profile
                .version
                .checked_add(1)
                .ok_or_else(|| AuthorizationStateError::Storage("version overflow".to_owned()))?;
            profile.updated_at = now;
            let mut idempotency = rpc_idempotency(
                "Auth.Deployments.BindParticipant",
                &caller.principal_id,
                input
                    .get("idempotencyKey")
                    .and_then(Value::as_str)
                    .unwrap_or(deployment_id),
                input,
                now,
            )?;
            idempotency.result = serde_json::to_value(&profile)
                .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
            self.service
                .repository()
                .put_deployment_profile(DeploymentProfileMutation {
                    profile: profile.clone(),
                    expected_version,
                    idempotency,
                    actions: Vec::new(),
                })
                .await?;
        }
        Ok(profile)
    }

    async fn service_instances_provision(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let deployment_id = required_string(&input, "deploymentId")?;
        let now = now_millis()?;
        let profile = self
            .bind_deployment_participant(
                deployment_id,
                nullable_string(&input, "participantId")?,
                PrincipalKind::Service,
                &input,
                caller,
                now,
            )
            .await?;
        let outcome = self
            .service
            .provision_service_identity(ProvisionServiceIdentityInput {
                deployment_id: deployment_id.to_owned(),
                instance_id: nullable_string(&input, "instanceId")?,
                identity_public_key: required_string(&input, "identityPublicKey")?.to_owned(),
                created_at: now,
                idempotency: rpc_idempotency(
                    "Auth.ServiceInstances.Provision",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        let identity = match outcome {
            IdempotentOutcome::Applied(identity) => identity,
            IdempotentOutcome::Replayed(value) => {
                let identity_key_id = value
                    .get("identityKeyId")
                    .and_then(Value::as_str)
                    .ok_or(AuthorizationStateError::StorageConflict)?;
                self.service
                    .repository()
                    .get_provisioned_identity(identity_key_id)
                    .await?
                    .ok_or(AuthorizationStateError::StorageConflict)?
            }
        };
        super::resources::ensure_operation_store(&self.client, deployment_id).await?;
        let instance = self
            .service
            .repository()
            .get_runtime_instance(&identity.instance_id)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        Ok(json!({
            "instance": service_instance_value(instance, identity, &profile),
        }))
    }

    async fn service_instances_list(
        &self,
        payload: &[u8],
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let identities = self
            .service
            .repository()
            .list_provisioned_identities()
            .await?;
        let mut entries = Vec::new();
        for identity in identities
            .into_iter()
            .filter(|identity| identity.kind == ProvisionedIdentityKind::Service)
        {
            if input
                .get("deploymentId")
                .and_then(Value::as_str)
                .is_some_and(|value| identity.deployment_id != value)
            {
                continue;
            }
            let Some(instance) = self
                .service
                .repository()
                .get_runtime_instance(&identity.instance_id)
                .await?
            else {
                continue;
            };
            if input
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|value| enum_string(instance.state) != value)
            {
                continue;
            }
            let profile = self
                .service
                .repository()
                .get_deployment_profile(&identity.deployment_id)
                .await?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            entries.push(service_instance_value(instance, identity, &profile));
        }
        paginate_values(
            entries,
            &input,
            "auth.ServiceInstances.List",
            &["/instanceId"],
        )
    }

    async fn devices_provision(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let deployment_id = required_string(&input, "deploymentId")?;
        let now = now_millis()?;
        let profile = self
            .bind_deployment_participant(
                deployment_id,
                nullable_string(&input, "participantId")?,
                PrincipalKind::Device,
                &input,
                caller,
                now,
            )
            .await?;
        let outcome = self
            .service
            .provision_device(ProvisionDeviceInput {
                deployment_id: deployment_id.to_owned(),
                instance_id: nullable_string(&input, "instanceId")?,
                identity_public_key: nullable_string(&input, "identityPublicKey")?,
                created_at: now,
                idempotency: rpc_idempotency(
                    "Auth.Devices.Provision",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        let (principal_id, instance_id, provisioning_secret) = match outcome {
            IdempotentOutcome::Applied(device) => (
                device.principal_id,
                device.instance_id,
                device.provisioning_secret,
            ),
            IdempotentOutcome::Replayed(value) => (
                required_string(&value, "principalId")?.to_owned(),
                required_string(&value, "instanceId")?.to_owned(),
                None,
            ),
        };
        Ok(json!({
            "device": self.device_value(&principal_id, &instance_id, &profile).await?,
            "provisioningSecret": provisioning_secret,
        }))
    }

    async fn devices_list(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let instances = self.service.repository().list_runtime_instances().await?;
        let mut entries = Vec::new();
        for device in self.service.repository().list_devices().await? {
            if input
                .get("deploymentId")
                .and_then(Value::as_str)
                .is_some_and(|value| device.deployment_id != value)
            {
                continue;
            }
            let Some(instance) = instances
                .iter()
                .find(|instance| instance.principal_id == device.principal_id)
            else {
                continue;
            };
            let profile = self
                .service
                .repository()
                .get_deployment_profile(&device.deployment_id)
                .await?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let value = self
                .device_value(&device.principal_id, &instance.instance_id, &profile)
                .await?;
            if input
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| value.get("state").and_then(Value::as_str) != Some(state))
            {
                continue;
            }
            entries.push(value);
        }
        paginate_values(
            entries,
            &input,
            "auth.Devices.List",
            &["/principalId", "/instanceId"],
        )
    }

    async fn provisioned_instance_set_state(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
        kind: ProvisionedIdentityKind,
        target: RuntimeInstanceState,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let instance_id = required_string(&input, "instanceId")?;
        let expected_version = required_u64(&input, "expectedVersion")?;
        let mut instance = self
            .service
            .repository()
            .get_runtime_instance(instance_id)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("instance not found".to_owned())
            })?;
        let mut identity = self
            .service
            .repository()
            .list_provisioned_identities()
            .await?
            .into_iter()
            .find(|identity| identity.instance_id == instance_id && identity.kind == kind);
        if kind == ProvisionedIdentityKind::Service && identity.is_none() {
            return Err(AuthorizationStateError::StorageConflict);
        }
        let now = now_millis()?;
        let device = if kind == ProvisionedIdentityKind::Device {
            let mut device = self
                .service
                .repository()
                .get_device(&instance.principal_id, &instance.deployment_id)
                .await?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            if device.version != expected_version
                || (device.state == crate::platform::auth::DeviceState::Pending
                    && target == RuntimeInstanceState::Active)
            {
                return Err(AuthorizationStateError::StorageConflict);
            }
            device.state = match target {
                RuntimeInstanceState::Active => crate::platform::auth::DeviceState::Active,
                RuntimeInstanceState::Disabled | RuntimeInstanceState::Stale => {
                    crate::platform::auth::DeviceState::Disabled
                }
                RuntimeInstanceState::Revoked => crate::platform::auth::DeviceState::Revoked,
            };
            device.updated_at = now;
            device.version = next_version(device.version)?;
            Some(device)
        } else {
            if instance.version != expected_version {
                return Err(AuthorizationStateError::StorageConflict);
            }
            None
        };
        instance.state = target;
        instance.updated_at = now;
        instance.version = next_version(instance.version)?;
        if let Some(identity) = identity.as_mut() {
            identity.state = match target {
                RuntimeInstanceState::Active => ProvisionedIdentityState::Active,
                RuntimeInstanceState::Disabled | RuntimeInstanceState::Stale => {
                    ProvisionedIdentityState::Active
                }
                RuntimeInstanceState::Revoked => ProvisionedIdentityState::Revoked,
            };
            identity.revoked_at = (target == RuntimeInstanceState::Revoked).then_some(now);
        }
        let action_kind = match kind {
            ProvisionedIdentityKind::Service => "ServiceInstances",
            ProvisionedIdentityKind::Device => "Devices",
        };
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        self.service
            .repository()
            .mutate_provisioned_instance(ProvisionedInstanceMutation {
                instance: instance.clone(),
                device: device.clone(),
                identity,
                expected_version,
                idempotency: rpc_idempotency(
                    &format!("Auth.{action_kind}.State"),
                    &caller.principal_id,
                    idempotency_key,
                    &input,
                    now,
                )?,
                actions: (target != RuntimeInstanceState::Active)
                    .then(|| PostCommitActionRecord {
                        predecessor_action_id: None,
                        action_id: digest_parts(&[instance_id, idempotency_key, "kick"]),
                        kind: PostCommitActionKind::Kick,
                        payload: json!({ "principalId": instance.principal_id }),
                        created_at: now,
                        attempts: 0,
                        next_attempt_at: now,
                        claimed_until: None,
                        last_error: None,
                    })
                    .into_iter()
                    .collect(),
            })
            .await?;
        let profile = self
            .service
            .repository()
            .get_deployment_profile(&instance.deployment_id)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        let mutation = json!({
            "resourceId": instance_id,
            "state": target,
            "version": device.as_ref().map_or(instance.version, |device| device.version),
            "changed": true,
        });
        match kind {
            ProvisionedIdentityKind::Service => {
                let identity = self
                    .service
                    .repository()
                    .list_provisioned_identities()
                    .await?
                    .into_iter()
                    .find(|identity| identity.instance_id == instance_id)
                    .ok_or(AuthorizationStateError::StorageConflict)?;
                Ok(json!({
                    "instance": service_instance_value(instance, identity, &profile),
                    "mutation": mutation,
                }))
            }
            ProvisionedIdentityKind::Device => Ok(json!({
                "device": self.device_value(
                    &instance.principal_id,
                    instance_id,
                    &profile,
                ).await?,
                "mutation": mutation,
            })),
        }
    }

    async fn device_value(
        &self,
        principal_id: &str,
        instance_id: &str,
        profile: &DeploymentProfileRecord,
    ) -> Result<Value, AuthorizationStateError> {
        let device = self
            .service
            .repository()
            .get_device(principal_id, &profile.deployment_id)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        let identity = self
            .service
            .repository()
            .list_provisioned_identities()
            .await?
            .into_iter()
            .find(|identity| {
                identity.principal_id == principal_id && identity.instance_id == instance_id
            });
        let delegation = self
            .service
            .repository()
            .get_device_delegation(principal_id, &profile.deployment_id)
            .await?;
        let state = match device.state {
            crate::platform::auth::DeviceState::Pending => "pending",
            crate::platform::auth::DeviceState::Active => "active",
            crate::platform::auth::DeviceState::Disabled => "disabled",
            crate::platform::auth::DeviceState::Revoked => "revoked",
        };
        Ok(json!({
            "instanceId": instance_id,
            "deploymentId": profile.deployment_id,
            "principalId": principal_id,
            "identityPublicKey": identity.as_ref().map(|value| value.identity_public_key.clone()),
            "identityKeyId": identity.as_ref().map(|value| value.identity_key_id.clone()),
            "participantId": profile.participant_id,
            "state": state,
            "administrativeApproval": match device.state {
                super::DeviceState::Pending => "pending",
                super::DeviceState::Active => "approved",
                super::DeviceState::Disabled => "approved",
                super::DeviceState::Revoked => "revoked",
            },
            "delegationRequired": profile.requires_device_delegation,
            "delegationState": delegation.as_ref().map_or(
                if profile.requires_device_delegation { "missing" } else { "active" },
                |value| match value.state {
                    super::DeviceDelegationState::Active => "active",
                    super::DeviceDelegationState::Missing => "missing",
                    super::DeviceDelegationState::Revoked => "revoked",
                },
            ),
            "delegationExpiresAt": delegation.and_then(|value| value.expires_at),
            "createdAt": device.created_at,
            "updatedAt": device.updated_at,
            "version": device.version,
        }))
    }

    async fn device_user_authorities_list(
        &self,
        payload: &[u8],
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let deployment_id = input.get("deploymentId").and_then(Value::as_str);
        let principal_id = input.get("principalId").and_then(Value::as_str);
        let identities = self
            .service
            .repository()
            .list_provisioned_identities()
            .await?;
        let mut entries = Vec::new();
        for device in self.service.repository().list_devices().await? {
            if deployment_id.is_some_and(|value| device.deployment_id != value)
                || principal_id.is_some_and(|value| device.principal_id != value)
            {
                continue;
            }
            let profile = self
                .service
                .repository()
                .get_deployment_profile(&device.deployment_id)
                .await?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            let identity = identities.iter().find(|identity| {
                identity.kind == ProvisionedIdentityKind::Device
                    && identity.principal_id == device.principal_id
                    && identity.deployment_id == device.deployment_id
            });
            let instance_id = identity
                .map(|identity| identity.instance_id.as_str())
                .ok_or(AuthorizationStateError::StorageConflict)?;
            entries.push(json!({
                "device": self.device_value(&device.principal_id, instance_id, &profile).await?,
            }));
        }
        paginate_values(
            entries,
            &input,
            "auth.DeviceUserAuthorities.List",
            &["/device/principalId", "/device/instanceId"],
        )
    }

    async fn device_user_authorities_revoke(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let deployment_id = required_string(&input, "deploymentId")?;
        let principal_id = required_string(&input, "devicePrincipalId")?;
        let mut device = self
            .service
            .repository()
            .get_device(principal_id, deployment_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("device not found".to_owned()))?;
        let mut delegation = self
            .service
            .repository()
            .get_device_delegation(principal_id, deployment_id)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("device delegation not found".to_owned())
            })?;
        let expected_version = device.version;
        let companion_session_id = delegation.user_login_session_id.clone();
        let now = now_millis()?;
        device.updated_at = now;
        device.version = next_version(device.version)?;
        delegation.state = DeviceDelegationState::Revoked;
        let identity = self
            .service
            .repository()
            .list_provisioned_identities()
            .await?
            .into_iter()
            .find(|identity| {
                identity.kind == ProvisionedIdentityKind::Device
                    && identity.principal_id == principal_id
                    && identity.deployment_id == deployment_id
            })
            .ok_or(AuthorizationStateError::StorageConflict)?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let action = |kind, suffix: &str, payload| PostCommitActionRecord {
            predecessor_action_id: None,
            action_id: digest_parts(&[
                "Auth.DeviceUserAuthorities.Revoke",
                idempotency_key,
                suffix,
            ]),
            kind,
            payload,
            created_at: now,
            attempts: 0,
            next_attempt_at: now,
            claimed_until: None,
            last_error: None,
        };
        let mut event_payload = json!({
            "eventType": "Auth.DeviceUserAuthorities.Resolved",
            "eventId": format!("evt_{}", digest_parts(&[principal_id, deployment_id, idempotency_key])),
            "occurredAt": now,
            "deploymentId": deployment_id,
            "instanceId": identity.instance_id.clone(),
            "state": "revoked",
        });
        event_payload["eventSubject"] = json!(auth_event_subject::<DeviceUserAuthoritiesResolved>(
            &event_payload
        )?);
        let mut actions = vec![action(PostCommitActionKind::Event, "event", event_payload)];
        if let Some(session_id) = &companion_session_id {
            actions.push(action(
                PostCommitActionKind::Kick,
                "kick",
                json!({ "sessionId": session_id }),
            ));
        }
        self.service
            .repository()
            .mutate_device_delegation(DeviceDelegationMutation {
                device: device.clone(),
                delegation: delegation.clone(),
                expected_version,
                idempotency: rpc_idempotency(
                    "Auth.DeviceUserAuthorities.Revoke",
                    &caller.principal_id,
                    idempotency_key,
                    &input,
                    now,
                )?,
                actions,
            })
            .await?;
        let profile = self
            .service
            .repository()
            .get_deployment_profile(deployment_id)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        let kicked_session_count = usize::from(companion_session_id.is_some());
        Ok(json!({
            "device": self.device_value(&device.principal_id, &identity.instance_id, &profile).await?,
            "kickedSessionCount": kicked_session_count,
        }))
    }

    async fn activation_reviews_list(
        &self,
        payload: &[u8],
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        self.service
            .expire_due_activation_reviews(now_millis()?)
            .await?;
        let entries = self
            .service
            .repository()
            .list_activation_reviews()
            .await?
            .into_iter()
            .filter(|review| {
                input
                    .get("deploymentId")
                    .and_then(Value::as_str)
                    .is_none_or(|value| review.deployment_id == value)
                    && input
                        .get("state")
                        .and_then(Value::as_str)
                        .is_none_or(|value| enum_string(review.state) == value)
            })
            .map(activation_review_value)
            .collect();
        paginate_values(
            entries,
            &input,
            "auth.DeviceUserAuthorities.Reviews.List",
            &["/reviewId"],
        )
    }

    async fn activation_reviews_decide(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let now = now_millis()?;
        self.service.expire_due_activation_reviews(now).await?;
        let review_id = required_string(&input, "reviewId")?;
        let review = self
            .service
            .repository()
            .get_activation_review(review_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("review not found".to_owned()))?;
        let state = match required_string(&input, "decision")? {
            "approve" => DeviceActivationReviewState::Approved,
            "reject" => DeviceActivationReviewState::Rejected,
            _ => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "activation decision is invalid".to_owned(),
                ));
            }
        };
        let profile = self
            .service
            .repository()
            .get_deployment_profile(&review.deployment_id)
            .await?
            .ok_or(AuthorizationStateError::StorageConflict)?;
        if profile.review_mode != Some(DeviceReviewMode::Required) {
            return Err(AuthorizationStateError::InvalidRecord(
                "deployment does not require administrative device review".to_owned(),
            ));
        }
        let event_suffix = if state == DeviceActivationReviewState::Approved {
            "approved"
        } else {
            "resolved"
        };
        let event_payload = if state == DeviceActivationReviewState::Approved {
            let mut payload = json!({
                "eventType": "Auth.DeviceUserAuthorities.Approved",
                "eventId": format!("evt_{}", digest_parts(&[review_id, event_suffix])),
                "occurredAt": now,
                "deploymentId": review.deployment_id,
                "instanceId": review.instance_id,
                "approvedBy": caller.principal_id,
            });
            payload["eventSubject"] = json!(auth_event_subject::<DeviceUserAuthoritiesApproved>(
                &payload
            )?);
            payload
        } else {
            let mut payload = json!({
                "eventType": "Auth.DeviceUserAuthorities.Resolved",
                "eventId": format!("evt_{}", digest_parts(&[review_id, event_suffix])),
                "occurredAt": now,
                "deploymentId": review.deployment_id,
                "instanceId": review.instance_id,
                "state": "rejected",
            });
            payload["eventSubject"] = json!(auth_event_subject::<DeviceUserAuthoritiesResolved>(
                &payload
            )?);
            payload
        };
        let mut actions = vec![PostCommitActionRecord {
            predecessor_action_id: Some(activation_review_event_action_id(
                review_id,
                if review.activated_by_user_principal_id.is_some() {
                    "requested"
                } else {
                    "review-requested"
                },
            )?),
            action_id: activation_review_event_action_id(review_id, event_suffix)?,
            kind: PostCommitActionKind::Event,
            payload: event_payload,
            created_at: now,
            attempts: 0,
            next_attempt_at: now,
            claimed_until: None,
            last_error: None,
        }];
        let activation_ready = state == DeviceActivationReviewState::Approved
            && (!(profile.requires_device_delegation
                || review.payload["companionRequired"].as_bool() == Some(true))
                || review.activated_by_user_principal_id.is_some());
        if activation_ready {
            let mut payload = json!({
                "eventType": "Auth.DeviceUserAuthorities.Resolved",
                "eventId": format!("evt_{}", digest_parts(&[review_id, "resolved"])),
                "occurredAt": now,
                "deploymentId": review.deployment_id,
                "instanceId": review.instance_id,
                "state": "active",
            });
            payload["eventSubject"] = json!(auth_event_subject::<DeviceUserAuthoritiesResolved>(
                &payload
            )?);
            actions.push(PostCommitActionRecord {
                predecessor_action_id: Some(activation_review_event_action_id(
                    review_id,
                    event_suffix,
                )?),
                action_id: activation_review_event_action_id(review_id, "resolved")?,
                kind: PostCommitActionKind::Event,
                payload,
                created_at: now,
                attempts: 0,
                next_attempt_at: now,
                claimed_until: None,
                last_error: None,
            });
        }
        let (delegation, companion_session) = if state == DeviceActivationReviewState::Approved {
            if let Some(user_principal_id) = review.activated_by_user_principal_id.as_deref() {
                crate::platform::auth_operation::companion_activation(
                    &self.service,
                    &review,
                    user_principal_id,
                    now,
                )
                .await?
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
        let outcome = self
            .service
            .decide_activation_review(DecideActivationReviewInput {
                review_id: review_id.to_owned(),
                expected_version: required_u64(&input, "expectedVersion")?,
                state,
                decided_by: caller.principal_id.clone(),
                reason: nullable_string(&input, "reason")?,
                delegation,
                companion_session,
                activate_device: activation_ready,
                decided_at: now,
                idempotency: rpc_idempotency(
                    "Auth.DeviceUserAuthorities.Reviews.Decide",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
                actions,
            })
            .await?;
        let review = match outcome {
            IdempotentOutcome::Applied(review) => review,
            IdempotentOutcome::Replayed(value) => {
                let review_id = required_string(&value, "reviewId")?;
                self.service
                    .repository()
                    .get_activation_review(review_id)
                    .await?
                    .ok_or(AuthorizationStateError::StorageConflict)?
            }
        };
        Ok(json!({ "review": activation_review_value(review) }))
    }

    async fn capability_groups_list(
        &self,
        payload: &[u8],
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let entries = self
            .service
            .repository()
            .list_capability_groups()
            .await?
            .into_iter()
            .map(|group| serde_json::to_value(group).expect("group serializes"))
            .collect();
        paginate_values(
            entries,
            &input,
            "auth.CapabilityGroups.List",
            &["/groupKey"],
        )
    }

    async fn capability_groups_get(
        &self,
        payload: &[u8],
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let group = self
            .service
            .repository()
            .get_capability_group(required_string(&input, "groupKey")?)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("capability group not found".to_owned())
            })?;
        Ok(json!({ "group": group }))
    }

    async fn capability_groups_put(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let key = required_string(&input, "groupKey")?;
        let now = now_millis()?;
        let current = self.service.repository().get_capability_group(key).await?;
        let expected_version = required_nullable_u64(&input, "expectedVersion")?;
        let mut capabilities = optional_string_array(&input, "capabilities")?.unwrap_or_default();
        capabilities.sort();
        capabilities.dedup();
        let mut included_groups =
            optional_string_array(&input, "includedGroups")?.unwrap_or_default();
        included_groups.sort();
        included_groups.dedup();
        for included_group in &included_groups {
            if included_group == key
                || self
                    .service
                    .repository()
                    .get_capability_group(included_group)
                    .await?
                    .is_none()
            {
                return Err(AuthorizationStateError::InvalidRecord(format!(
                    "unknown included capability group '{included_group}'"
                )));
            }
        }
        let semantic_changed = current.as_ref().is_none_or(|group| {
            group.capabilities != capabilities || group.included_groups != included_groups
        });
        let group = CapabilityGroupRecord {
            group_key: key.to_owned(),
            display_name: required_string(&input, "displayName")?.to_owned(),
            description: required_string(&input, "description")?.to_owned(),
            capabilities,
            included_groups,
            created_at: current.as_ref().map_or(now, |group| group.created_at),
            updated_at: now,
            version: expected_version.map_or(Ok(1), next_version)?,
        };
        let outcome = self
            .service
            .repository()
            .put_capability_group(
                group,
                expected_version,
                rpc_idempotency(
                    "Auth.CapabilityGroups.Put",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
            )
            .await?;
        if semantic_changed {
            self.portal_reconciliation.notify_all();
        }
        let group = match outcome {
            IdempotentOutcome::Applied(group) => serde_json::to_value(group),
            IdempotentOutcome::Replayed(group) => Ok(group),
        }
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        Ok(json!({ "group": group }))
    }

    async fn capability_groups_delete(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let key = required_string(&input, "groupKey")?;
        let now = now_millis()?;
        let outcome = self
            .service
            .repository()
            .delete_capability_group(
                key,
                required_u64(&input, "expectedVersion")?,
                rpc_idempotency(
                    "Auth.CapabilityGroups.Delete",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
            )
            .await?;
        self.portal_reconciliation.notify_all();
        let success = match outcome {
            IdempotentOutcome::Applied(success) => success,
            IdempotentOutcome::Replayed(success) => success.as_bool().ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("invalid delete replay".to_owned())
            })?,
        };
        Ok(json!({ "success": success }))
    }

    async fn portal_grant_overrides_list(
        &self,
        payload: &[u8],
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let entries = self
            .service
            .repository()
            .list_portal_grant_overrides(
                input.get("portalId").and_then(Value::as_str),
                input.get("participantId").and_then(Value::as_str),
            )
            .await?
            .into_iter()
            .map(|entry| serde_json::to_value(entry).expect("policy serializes"))
            .collect();
        paginate_values(
            entries,
            &input,
            "auth.Portals.GrantOverrides.List",
            &["/portalId", "/participantId"],
        )
    }

    async fn portal_grant_overrides_put(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        require_admin(caller)?;
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let portal_id = required_string(&input, "portalId")?;
        let participant_id = required_string(&input, "participantId")?;
        let expected_version = required_nullable_u64(&input, "expectedVersion")?;
        let current = self
            .service
            .repository()
            .get_portal_grant_override(portal_id, participant_id)
            .await?;
        let mut direct_capabilities =
            optional_string_array(&input, "directCapabilities")?.unwrap_or_default();
        direct_capabilities.sort();
        direct_capabilities.dedup();
        let mut capability_group_keys =
            optional_string_array(&input, "capabilityGroupKeys")?.unwrap_or_default();
        capability_group_keys.sort();
        capability_group_keys.dedup();
        let mut role_mappings = input
            .get("roleMappings")
            .cloned()
            .map(serde_json::from_value::<Vec<PortalRoleMapping>>)
            .transpose()
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?
            .unwrap_or_default();
        for mapping in &mut role_mappings {
            mapping.direct_capabilities.sort();
            mapping.direct_capabilities.dedup();
            mapping.capability_group_keys.sort();
            mapping.capability_group_keys.dedup();
        }
        sort_and_validate_role_mappings(&mut role_mappings)?;
        let now = now_millis()?;
        let policy = PortalGrantOverrideRecord {
            portal_id: portal_id.to_owned(),
            participant_id: participant_id.to_owned(),
            direct_capabilities,
            capability_group_keys,
            role_mappings,
            created_at: current.as_ref().map_or(now, |policy| policy.created_at),
            updated_at: now,
            version: expected_version.map_or(Ok(1), next_version)?,
        };
        let outcome = self
            .service
            .repository()
            .put_portal_grant_override(
                policy,
                expected_version,
                rpc_idempotency(
                    "Auth.Portals.GrantOverrides.Put",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
            )
            .await?;
        self.portal_reconciliation.notify_portal(portal_id).await;
        let policy = match outcome {
            IdempotentOutcome::Applied(policy) => serde_json::to_value(policy),
            IdempotentOutcome::Replayed(policy) => Ok(policy),
        }
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        Ok(json!({ "policy": policy }))
    }

    async fn portal_grant_overrides_remove(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        require_admin(caller)?;
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let now = now_millis()?;
        let outcome = self
            .service
            .repository()
            .remove_portal_grant_override(
                required_string(&input, "portalId")?,
                required_string(&input, "participantId")?,
                required_u64(&input, "expectedVersion")?,
                rpc_idempotency(
                    "Auth.Portals.GrantOverrides.Remove",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
            )
            .await?;
        self.portal_reconciliation
            .notify_portal(required_string(&input, "portalId")?)
            .await;
        let removed: Option<super::PortalGrantOverrideRecord> = match outcome {
            IdempotentOutcome::Applied(policy) => policy,
            IdempotentOutcome::Replayed(policy) => serde_json::from_value(policy)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        };
        Ok(removed.map_or_else(|| json!({}), |removed| json!({ "removed": removed })))
    }

    async fn sessions_me(
        &self,
        validated: ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let now = now_millis()?;
        let issued = self
            .service
            .repository()
            .get_context_by_digest(validated.context.context_digest())
            .await?
            .ok_or(AuthorizationStateError::NotAuthorized)?;
        if issued.state != super::context::AuthorizationContextState::Active {
            return Err(AuthorizationStateError::NotAuthorized);
        }
        // The runtime proof key is the key bound into the issued context, which
        // may differ from the durable login credential that authorized it.
        if issued.principal_id != validated.principal_id
            || issued.participant_id != validated.context.participant_id()
            || issued.session_public_key != validated.session_public_key
        {
            return Err(AuthorizationStateError::NotAuthorized);
        }
        let installed = self
            .service
            .repository()
            .get_installed_participant(issued.participant_id, Some(issued.installed_revision))
            .await?;
        let session = match validated.context.login_session_id() {
            Some(id) => {
                let session = self
                    .service
                    .repository()
                    .get_session(id)
                    .await?
                    .ok_or(AuthorizationStateError::SessionMissing)?;
                if session.principal_id != validated.principal_id
                    || session.participant_id != validated.context.participant_id()
                {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
                if session.state == SessionState::Revoked {
                    return Err(AuthorizationStateError::SessionRevoked);
                }
                if session.state == SessionState::Expired
                    || session.expires_at.is_some_and(|expires| expires <= now)
                {
                    return Err(AuthorizationStateError::SessionExpired);
                }
                Some(session)
            }
            None => None,
        };
        let user = if validated.principal_kind == PrincipalKind::User {
            let (principal, profile) = self
                .service
                .repository()
                .get_user_account(&validated.principal_id)
                .await?
                .ok_or(AuthorizationStateError::PrincipalMissing)?;
            if principal.state != PrincipalState::Active {
                return Err(AuthorizationStateError::PrincipalInactive);
            }
            Some(user_value(UserAccount { principal, profile }))
        } else {
            None
        };
        let mut connection = json!({
            "connectionId": validated.context.connection_id(),
            "sessionKey": validated.session_public_key,
            "inboxPrefix": validated.context.inbox_prefix(),
            "participantId": validated.context.participant_id(),
            "participantKind": installed["participant"]["participantKind"],
            "principalId": validated.principal_id,
            "principalKind": validated.principal_kind,
            "grants": validated.context.grant_set(),
            "platformPrivileges": validated.context.platform_privileges(),
        });
        for (field, value) in [
            ("loginSessionId", validated.context.login_session_id()),
            ("identityKeyId", validated.context.identity_key_id()),
            ("deploymentId", validated.context.deployment_id()),
            ("instanceId", validated.context.instance_id()),
        ] {
            if let Some(value) = value {
                connection[field] = json!(value);
            }
        }
        Ok(json!({
            "session": session,
            "user": user,
            "connection": connection,
        }))
    }

    async fn connections_list(
        &self,
        payload: &[u8],
        _validated: ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let session_id = input.get("sessionId").and_then(Value::as_str);
        let entries = self
            .ephemeral
            .list_connection_presence(session_id)
            .await?
            .into_iter()
            .map(connection_value)
            .collect::<Vec<_>>();
        paginate_values(entries, &input, "auth.Connections.List", &["/connectionId"])
    }

    async fn portals_list(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let mut entries = Vec::new();
        for portal in self.service.repository().list_login_portals().await? {
            if portal.removed {
                continue;
            }
            let (_, settings) = self
                .service
                .repository()
                .get_login_portal(&portal.portal_id)
                .await?
                .ok_or(AuthorizationStateError::StorageConflict)?;
            entries.push(portal_value(portal, settings));
        }
        paginate_values(entries, &input, "auth.Portals.List", &["/portalId"])
    }

    async fn portals_get(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let portal_id = required_string(&input, "portalId")?;
        let (portal, settings) = self
            .service
            .repository()
            .get_login_portal(portal_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("portal not found".to_owned()))?;
        if portal.removed {
            return Err(AuthorizationStateError::InvalidRecord(
                "portal not found".to_owned(),
            ));
        }
        let routes = self
            .service
            .repository()
            .list_portal_routes()
            .await?
            .into_iter()
            .filter(|route| route.portal_id == portal_id)
            .collect::<Vec<_>>();
        Ok(json!({ "portal": portal_value(portal, settings), "routes": routes }))
    }

    async fn portals_put(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let portal_id = required_string(&input, "portalId")?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let current = self
            .service
            .repository()
            .get_login_portal(portal_id)
            .await?;
        let expected_version = required_nullable_u64(&input, "expectedVersion")?;
        let now = now_millis()?;
        let settings_value = input.get("loginSettings").ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("loginSettings is required".to_owned())
        })?;
        let provider_ids =
            optional_string_array(settings_value, "providers")?.unwrap_or_else(|| {
                current
                    .as_ref()
                    .map_or_else(Vec::new, |value| value.0.provider_ids.clone())
            });
        let version = expected_version.map_or(Ok(1), next_version)?;
        let portal = LoginPortalRecord {
            portal_id: portal_id.to_owned(),
            display_name: required_string(&input, "displayName")?.to_owned(),
            entry_url: nullable_string(&input, "entryUrl")?,
            builtin: current.as_ref().is_some_and(|value| value.0.builtin),
            disabled: required_bool(&input, "disabled")?,
            removed: false,
            local_registration_enabled: required_bool(settings_value, "localRegistration")?,
            provider_ids: provider_ids.clone(),
            created_at: current.as_ref().map_or(now, |value| value.0.created_at),
            updated_at: now,
            version,
        };
        let settings =
            login_settings_from_value(portal_id, settings_value, provider_ids, now, version)?;
        let result = json!({ "portal": portal_value(portal.clone(), settings.clone()) });
        let mut idempotency = rpc_idempotency(
            "Auth.Portals.Put",
            &caller.principal_id,
            idempotency_key,
            &input,
            now,
        )?;
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .put_login_portal(LoginPortalMutation {
                portal,
                settings,
                expected_version,
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        self.portal_reconciliation.notify_portal(portal_id).await;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => result,
            IdempotentOutcome::Replayed(value) => value,
        })
    }

    async fn portals_remove(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let portal_id = required_string(&input, "portalId")?;
        let (mut portal, mut settings) = self
            .service
            .repository()
            .get_login_portal(portal_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("portal not found".to_owned()))?;
        if portal.builtin {
            return Err(AuthorizationStateError::InvalidRecord(
                "the built-in portal cannot be removed".to_owned(),
            ));
        }
        let expected_version = required_u64(&input, "expectedVersion")?;
        let now = now_millis()?;
        portal.removed = true;
        portal.updated_at = now;
        portal.version = next_version(portal.version)?;
        settings.updated_at = now;
        settings.version = portal.version;
        let result = json!({ "removed": true });
        let mut idempotency = rpc_idempotency(
            "Auth.Portals.Remove",
            &caller.principal_id,
            required_string(&input, "idempotencyKey")?,
            &input,
            now,
        )?;
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .put_login_portal(LoginPortalMutation {
                portal,
                expected_version: Some(expected_version),
                settings,
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        self.portal_reconciliation.notify_portal(portal_id).await;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => result,
            IdempotentOutcome::Replayed(value) => value,
        })
    }

    async fn capabilities_list(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let source_api = input.get("sourceApi").and_then(Value::as_str);
        let entries = if source_api
            .is_none_or(|value| value == trellis_runtime_apis::apis::trellis_auth_v1::API_ID)
        {
            let (_, participant) = self
                .service
                .repository()
                .get_installed_participant_record(
                    super::builtins::AUTH_RUNTIME_PARTICIPANT_ID.to_owned(),
                    None,
                )
                .await?
                .ok_or(AuthorizationStateError::ParticipantMissing)?;
            participant
                .projection
                .implemented_apis
                .get(trellis_runtime_apis::apis::trellis_auth_v1::API_ID)
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "installed Auth API projection is absent".to_owned(),
                    )
                })?
                .capabilities
                .iter()
                .map(|(capability, definition)| {
                    json!({
                        "capability": capability,
                        "displayName": definition.display_name,
                        "description": definition.description,
                        "allows": definition.allows,
                        "sourceApi": trellis_runtime_apis::apis::trellis_auth_v1::API_ID,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        paginate_values(
            entries,
            &input,
            "auth.Capabilities.List",
            &["/sourceApi", "/capability"],
        )
    }

    async fn portal_settings_get(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let portal_id = required_string(&input, "portalId")?;
        let (portal, settings) = self
            .service
            .repository()
            .get_login_portal(portal_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("portal not found".to_owned()))?;
        if portal.removed {
            return Err(AuthorizationStateError::InvalidRecord(
                "portal not found".to_owned(),
            ));
        }
        Ok(json!({
            "portalId": portal_id,
            "settings": login_settings_value(&portal, &settings),
            "version": settings.version,
        }))
    }

    async fn portal_settings_update(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let portal_id = required_string(&input, "portalId")?;
        let expected_version = required_u64(&input, "expectedVersion")?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let (mut portal, _) = self
            .service
            .repository()
            .get_login_portal(portal_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("portal not found".to_owned()))?;
        let settings_value = input.get("settings").ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("settings is required".to_owned())
        })?;
        let provider_ids = optional_string_array(settings_value, "providers")?
            .unwrap_or_else(|| portal.provider_ids.clone());
        let now = now_millis()?;
        let version = next_version(expected_version)?;
        portal.provider_ids = provider_ids.clone();
        portal.local_registration_enabled = required_bool(settings_value, "localRegistration")?;
        portal.updated_at = now;
        portal.version = version;
        let settings =
            login_settings_from_value(portal_id, settings_value, provider_ids, now, version)?;
        let result = json!({
            "portalId": portal_id,
            "settings": login_settings_value(&portal, &settings),
            "version": settings.version,
        });
        let mut idempotency = rpc_idempotency(
            "Auth.Portals.LoginSettings.Update",
            &caller.principal_id,
            idempotency_key,
            &input,
            now,
        )?;
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .put_login_portal(LoginPortalMutation {
                portal,
                settings,
                expected_version: Some(expected_version),
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        self.portal_reconciliation.notify_portal(portal_id).await;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => result,
            IdempotentOutcome::Replayed(value) => value,
        })
    }

    async fn portal_route_put(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let expected_version = required_nullable_u64(&input, "expectedVersion")?;
        let route_id = input
            .get("routeId")
            .and_then(Value::as_str)
            .map_or_else(|| format!("ptr_{}", Ulid::new()), str::to_owned);
        let routes = self.service.repository().list_portal_routes().await?;
        let current = routes
            .iter()
            .find(|route| route.route_id == route_id)
            .cloned();
        let now = now_millis()?;
        let route = PortalRouteRecord {
            route_id: route_id.clone(),
            portal_id: required_string(&input, "portalId")?.to_owned(),
            participant_id: nullable_string(&input, "participantId")?,
            origin: nullable_string(&input, "origin")?,
            deployment_id: nullable_string(&input, "deploymentId")?,
            priority: required_i64(&input, "priority")?,
            created_at: current.as_ref().map_or(now, |route| route.created_at),
            updated_at: now,
            version: expected_version.map_or(Ok(1), next_version)?,
        };
        let result = json!({ "route": route });
        let mut idempotency = rpc_idempotency(
            "Auth.Portals.Routes.Put",
            &caller.principal_id,
            required_string(&input, "idempotencyKey")?,
            &input,
            now,
        )?;
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .put_portal_route(PortalRouteMutation {
                route,
                expected_version,
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => result,
            IdempotentOutcome::Replayed(value) => value,
        })
    }

    async fn portal_route_remove(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let route_id = required_string(&input, "routeId")?;
        let now = now_millis()?;
        let result = json!({ "routeId": route_id, "removed": true });
        let mut idempotency = rpc_idempotency(
            "Auth.Portals.Routes.Remove",
            &caller.principal_id,
            required_string(&input, "idempotencyKey")?,
            &input,
            now,
        )?;
        idempotency.result = result.clone();
        let outcome = self
            .service
            .repository()
            .remove_portal_route(PortalRouteRemoval {
                route_id: route_id.to_owned(),
                expected_version: required_u64(&input, "expectedVersion")?,
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        Ok(match outcome {
            IdempotentOutcome::Applied(_) => result,
            IdempotentOutcome::Replayed(value) => value,
        })
    }

    async fn sessions_list(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let request: trellis_runtime_apis::types::AuthSessionsListRequest =
            serde_json::from_slice(payload)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        if request
            .page
            .as_ref()
            .and_then(|page| page.limit)
            .is_some_and(|limit| !(1..=100).contains(&limit))
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "invalid page limit".into(),
            ));
        }
        let mut input = serde_json::to_value(request)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        if !caller_is_admin(caller) {
            if nullable_string(&input, "principalId")?
                .is_some_and(|principal_id| principal_id != caller.principal_id)
            {
                return Err(AuthorizationStateError::NotAuthorized);
            }
            input["principalId"] = json!(caller.principal_id);
        }
        let mut entries = self.service.repository().list_sessions().await?;
        entries.retain(|session| {
            input
                .get("principalId")
                .and_then(Value::as_str)
                .is_none_or(|value| session.principal_id == value)
                && input
                    .get("participantId")
                    .and_then(Value::as_str)
                    .is_none_or(|value| session.participant_id == value)
                && input
                    .get("state")
                    .and_then(Value::as_str)
                    .is_none_or(|value| {
                        serde_json::to_value(session.state)
                            .ok()
                            .and_then(|state| state.as_str().map(str::to_owned))
                            .as_deref()
                            == Some(value)
                    })
        });
        entries.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        paginate_sessions(entries, &input)
    }

    async fn sessions_logout(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        if !input.is_object() {
            return Err(AuthorizationStateError::InvalidRecord(
                "logout request must be an object".to_owned(),
            ));
        }
        let login_session_id = caller
            .context
            .login_session_id()
            .ok_or(AuthorizationStateError::WrongPrincipalKind)?;
        let session = self
            .service
            .repository()
            .get_session(login_session_id)
            .await?
            .ok_or(AuthorizationStateError::SessionMissing)?;
        let input = json!({
            "sessionId": login_session_id,
            "expectedVersion": session.version.to_string(),
            "idempotencyKey": "logout",
            "reason": null,
        });
        self.sessions_revoke(
            &serde_json::to_vec(&input)
                .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
            Some(caller),
        )
        .await
    }

    async fn sessions_revoke(
        &self,
        payload: &[u8],
        caller: Option<&ValidatedRequest>,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let session_id = required_string(&input, "sessionId")?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let session = self
            .service
            .repository()
            .get_session(session_id)
            .await?
            .ok_or(AuthorizationStateError::SessionMissing)?;
        let expected_version = required_u64(&input, "expectedVersion")?;
        let now = now_millis()?;
        let connections = self
            .ephemeral
            .list_connection_presence(Some(session_id))
            .await?;
        let kicked_connections = connections.len();
        let request_digest = trellis_protocol::digest_json(&input)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let action = |kind, suffix: &str, payload| PostCommitActionRecord {
            predecessor_action_id: None,
            action_id: digest_parts(&[session_id, idempotency_key, suffix]),
            kind,
            payload,
            created_at: now,
            attempts: 0,
            next_attempt_at: now,
            claimed_until: None,
            last_error: None,
        };
        let mut event_payload = json!({
            "eventType": "Auth.Sessions.Revoked",
            "eventId": format!("evt_{}", digest_parts(&[session_id, idempotency_key])),
            // Generated uint64 event fields travel as decimal strings.
            "occurredAt": now.to_string(),
            "sessionId": session_id,
            "principalId": session.principal_id.clone(),
            "participantId": session.participant_id.clone(),
            "reason": input.get("reason"),
            "revokedBy": caller.map(|caller| &caller.principal_id),
        });
        event_payload["eventSubject"] =
            json!(auth_event_subject::<SessionsRevoked>(&event_payload)?);
        let outcome = self
            .service
            .revoke_session(
                session_id.to_owned(),
                expected_version,
                now,
                IdempotencyResultRecord {
                    scope_key: digest_parts(&["Auth.Sessions.Revoke", session_id]),
                    purpose: "Auth.Sessions.Revoke".to_owned(),
                    signer_id: caller
                        .map(|caller| caller.principal_id.clone())
                        .unwrap_or_else(|| "rpc".to_owned()),
                    request_id: idempotency_key.to_owned(),
                    request_digest,
                    result: Value::Null,
                    created_at: now,
                    expires_at: now.saturating_add(86_400_000),
                },
                vec![
                    action(PostCommitActionKind::Event, "event", event_payload),
                    action(
                        PostCommitActionKind::Kick,
                        "kick",
                        json!({
                            "sessionId": session_id,
                            "connections": connections,
                            "reason": input.get("reason").and_then(Value::as_str),
                        }),
                    ),
                ],
            )
            .await?;
        let session = match outcome {
            IdempotentOutcome::Applied(session) => session,
            IdempotentOutcome::Replayed(_) => self
                .service
                .repository()
                .get_session(session_id)
                .await?
                .ok_or(AuthorizationStateError::SessionMissing)?,
        };
        Ok(json!({
            "session": session,
            "kickedConnections": kicked_connections,
        }))
    }

    async fn connections_kick(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let connection_id = required_string(&input, "connectionId")?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        self.ephemeral
            .list_connection_presence(None)
            .await?
            .into_iter()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("connection not found".to_owned())
            })?;
        let now = now_millis()?;
        let request_digest = trellis_protocol::digest_json(&input)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        self.service
            .repository()
            .enqueue_idempotent_post_commit_actions(
                IdempotencyResultRecord {
                    scope_key: digest_parts(&["Auth.Connections.Kick", connection_id]),
                    purpose: "Auth.Connections.Kick".to_owned(),
                    signer_id: caller.principal_id.clone(),
                    request_id: idempotency_key.to_owned(),
                    request_digest,
                    result: json!({ "connectionId": connection_id, "kicked": true }),
                    created_at: now,
                    expires_at: now.saturating_add(86_400_000),
                },
                vec![PostCommitActionRecord {
                    predecessor_action_id: None,
                    action_id: digest_parts(&[
                        "Auth.Connections.Kick",
                        connection_id,
                        idempotency_key,
                    ]),
                    kind: PostCommitActionKind::Kick,
                    payload: json!({
                        "connectionId": connection_id,
                        "reason": input.get("reason").and_then(Value::as_str),
                    }),
                    created_at: now,
                    attempts: 0,
                    next_attempt_at: now,
                    claimed_until: None,
                    last_error: None,
                }],
            )
            .await?;
        Ok(json!({ "connectionId": connection_id, "kicked": true }))
    }

    async fn users_create(
        &self,
        payload: &[u8],
        validated: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let now = now_millis()?;
        let outcome = self
            .service
            .create_user(CreateUserInput {
                name: nullable_string(&input, "name")?,
                email: nullable_string(&input, "email")?,
                image: nullable_string(&input, "image")?,
                username: nullable_string(&input, "username")?,
                created_at: now,
                idempotency: rpc_idempotency(
                    "Auth.Users.Create",
                    &validated.principal_id,
                    idempotency_key,
                    &input,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        let account = match outcome {
            IdempotentOutcome::Applied(account) => account,
            IdempotentOutcome::Replayed(result) => {
                let principal_id = required_string(&result, "principalId")?;
                self.service
                    .user(principal_id)
                    .await?
                    .ok_or(AuthorizationStateError::PrincipalMissing)?
            }
        };
        Ok(json!({ "user": user_value(account) }))
    }

    async fn password_change(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let login_session_id = caller
            .context
            .login_session_id()
            .ok_or(AuthorizationStateError::WrongPrincipalKind)?;
        let now = now_millis()?;
        let action = PostCommitActionRecord {
            predecessor_action_id: None,
            action_id: digest_parts(&[
                "Auth.Users.Password.Change",
                &caller.principal_id,
                required_string(&input, "idempotencyKey")?,
            ]),
            kind: PostCommitActionKind::Kick,
            payload: json!({
                "principalId": caller.principal_id,
                "exceptSessionId": login_session_id,
            }),
            created_at: now,
            attempts: 0,
            next_attempt_at: now,
            claimed_until: None,
            last_error: None,
        };
        match self
            .service
            .change_password(ChangePasswordInput {
                principal_id: caller.principal_id.clone(),
                current_session_id: login_session_id.to_owned(),
                current_password: required_string(&input, "currentPassword")?.to_owned(),
                new_password: required_string(&input, "newPassword")?.to_owned(),
                changed_at: now,
                idempotency: rpc_idempotency(
                    "Auth.Users.Password.Change",
                    &caller.principal_id,
                    required_string(&input, "idempotencyKey")?,
                    &input,
                    now,
                )?,
                actions: vec![action],
            })
            .await?
        {
            IdempotentOutcome::Applied(revoked) => Ok(json!({
                "changedAt": now,
                "revokedSessionCount": revoked,
            })),
            IdempotentOutcome::Replayed(value) => Ok(value),
        }
    }

    async fn password_reset_create(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let principal_id = required_string(&input, "userId")?;
        self.service
            .user(principal_id)
            .await?
            .ok_or_else(|| AuthorizationStateError::InvalidRecord("user not found".to_owned()))?;
        self.create_rpc_account_flow(
            &input,
            caller,
            AccountFlowKind::PasswordReset,
            principal_id,
            Vec::new(),
            "Auth.Users.PasswordReset.Create",
        )
        .await
    }

    async fn identity_link_create(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let allowed_providers = required_string_array(&input, "allowedProviders")?;
        self.create_rpc_account_flow(
            &input,
            caller,
            AccountFlowKind::IdentityLink,
            &caller.principal_id,
            allowed_providers,
            "Auth.Users.IdentityLink.Create",
        )
        .await
    }

    async fn create_rpc_account_flow(
        &self,
        input: &Value,
        caller: &ValidatedRequest,
        kind: AccountFlowKind,
        principal_id: &str,
        allowed_providers: Vec<String>,
        purpose: &str,
    ) -> Result<Value, AuthorizationStateError> {
        let now = now_millis()?;
        let admin_target = if kind == AccountFlowKind::PasswordReset {
            self.require_admin_for_admin_target(caller, principal_id)
                .await?
        } else {
            false
        };
        let return_target = nullable_string(input, "returnTarget")?;
        let outcome = self
            .service
            .create_account_flow(CreateAccountFlowInput {
                kind,
                target_principal_id: Some(principal_id.to_owned()),
                target_provider_id: None,
                return_location: return_target.clone(),
                payload: json!({
                    "allowedProviders": allowed_providers,
                    "adminTarget": admin_target,
                    "requestedByPrincipalId": caller.principal_id,
                }),
                created_at: now,
                expires_at: now.saturating_add(15 * 60_000),
                idempotency: rpc_idempotency(
                    purpose,
                    &caller.principal_id,
                    required_string(input, "idempotencyKey")?,
                    input,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        let flow = match outcome {
            IdempotentOutcome::Applied(flow) => flow,
            IdempotentOutcome::Replayed(_) => {
                return Err(AuthorizationStateError::StorageConflict);
            }
        };
        let kind = match kind {
            AccountFlowKind::PasswordReset => "password_reset",
            AccountFlowKind::IdentityLink => "identity_link",
            _ => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "unsupported RPC account flow".to_owned(),
                ));
            }
        };
        Ok(json!({
            "flow": {
                "flowId": flow.flow_id,
                "kind": kind,
                "targetPrincipalId": principal_id,
                "allowedProviders": allowed_providers,
                "returnTarget": return_target,
                "createdAt": now,
                "expiresAt": flow.expires_at,
                "consumedAt": null,
                "version": 1,
                "completionUrl": format!(
                    "{}/auth/account-flow/{}",
                    self.public_origin.trim_end_matches('/'),
                    flow.token
                ),
            }
        }))
    }

    async fn user_identities_list(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let provider = input.get("providerId").and_then(Value::as_str);
        let entries = self
            .service
            .repository()
            .list_provider_identities(&caller.principal_id)
            .await?
            .into_iter()
            .filter(|identity| provider.is_none_or(|value| identity.provider == value))
            .map(|identity| {
                json!({
                    "providerId": identity.provider,
                    "subject": identity.provider_subject,
                    "principalId": identity.principal_id,
                    "username": null,
                    "observedName": null,
                    "observedEmail": null,
                    "createdAt": identity.linked_at,
                    "lastSeenAt": identity.last_seen_at,
                })
            })
            .collect();
        paginate_values(
            entries,
            &input,
            "auth.UserIdentities.List",
            &["/providerId", "/subject"],
        )
    }

    async fn user_identities_unlink(
        &self,
        payload: &[u8],
        caller: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let now = now_millis()?;
        let mut idempotency = rpc_idempotency(
            "Auth.UserIdentities.Unlink",
            &caller.principal_id,
            required_string(&input, "idempotencyKey")?,
            &input,
            now,
        )?;
        idempotency.result = json!({ "unlinked": true });
        let outcome = self
            .service
            .repository()
            .unlink_provider_identity(ProviderIdentityUnlink {
                provider: required_string(&input, "providerId")?.to_owned(),
                provider_subject: required_string(&input, "subject")?.to_owned(),
                principal_id: caller.principal_id.clone(),
                idempotency,
                actions: Vec::new(),
            })
            .await?;
        match outcome {
            IdempotentOutcome::Applied(unlinked) => Ok(json!({ "unlinked": unlinked })),
            IdempotentOutcome::Replayed(value) => Ok(value),
        }
    }

    async fn users_get(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let account = self
            .service
            .user(required_string(&input, "userId")?)
            .await?
            .ok_or(AuthorizationStateError::PrincipalMissing)?;
        Ok(json!({ "user": user_value(account) }))
    }

    async fn users_resolve(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let selector = input.get("selector").ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("selector is required".to_owned())
        })?;
        let principal_id = match selector.get("kind").and_then(Value::as_str) {
            Some("user") => required_string(selector, "userId")?.to_owned(),
            Some("provider") => self
                .service
                .repository()
                .get_provider_identity(
                    required_string(selector, "providerId")?,
                    required_string(selector, "providerSubject")?,
                )
                .await?
                .map(|identity| identity.principal_id)
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord("user not found".to_owned())
                })?,
            _ => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "selector kind is invalid".to_owned(),
                ));
            }
        };
        let account =
            self.service.user(&principal_id).await?.ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("user not found".to_owned())
            })?;
        Ok(json!({ "user": user_value(account) }))
    }

    async fn users_list(&self, payload: &[u8]) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let limit = input
            .pointer("/page/limit")
            .and_then(Value::as_u64)
            .unwrap_or(50);
        if limit == 0 || limit > 200 {
            return Err(AuthorizationStateError::InvalidRecord(
                "invalid pagination limit".to_owned(),
            ));
        }
        let limit = limit as usize;
        let mut query = input.clone();
        query.as_object_mut().map(|query| query.remove("page"));
        let query_digest = trellis_protocol::pagination_query_digest("auth.Users.List", &query)
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        let after = input
            .pointer("/page/cursor")
            .and_then(Value::as_str)
            .map(|cursor| {
                trellis_protocol::decode_pagination_cursor::<(i64, String)>(cursor, &query_digest)
            })
            .transpose()
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        let state = input.get("state").and_then(Value::as_str);
        let search = input.get("search").and_then(Value::as_str);
        let mut accounts = self
            .service
            .users(after.as_ref(), state, search, limit + 1)
            .await?;
        let next_cursor = (accounts.len() > limit)
            .then(|| {
                let account = &accounts[limit - 1];
                (
                    account.principal.created_at,
                    account.principal.principal_id.clone(),
                )
            })
            .map(|after| trellis_protocol::encode_pagination_cursor(&query_digest, &after))
            .transpose()
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
        accounts.truncate(limit);
        let page = next_cursor.map_or_else(|| json!({}), |cursor| json!({"nextCursor": cursor}));
        Ok(json!({
            "items": accounts.into_iter().map(user_value).collect::<Vec<_>>(),
            "page": page,
        }))
    }

    async fn users_update(
        &self,
        payload: &[u8],
        validated: &ValidatedRequest,
    ) -> Result<Value, AuthorizationStateError> {
        let input: Value = serde_json::from_slice(payload)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let principal_id = required_string(&input, "userId")?;
        let idempotency_key = required_string(&input, "idempotencyKey")?;
        let expected_version = required_u64(&input, "expectedVersion")?;
        let state = match required_string(&input, "state")? {
            "active" => PrincipalState::Active,
            "disabled" => PrincipalState::Disabled,
            _ => {
                return Err(AuthorizationStateError::InvalidRecord(
                    "user state must be active or disabled".to_owned(),
                ));
            }
        };
        let now = now_millis()?;
        let outcome = self
            .service
            .update_user(UpdateUserInput {
                actor: mutation_actor(validated),
                principal_id: principal_id.to_owned(),
                expected_version,
                name: nullable_string(&input, "name")?,
                email: nullable_string(&input, "email")?,
                image: nullable_string(&input, "image")?,
                state,
                updated_at: now,
                idempotency: rpc_idempotency(
                    "Auth.Users.Update",
                    &validated.principal_id,
                    idempotency_key,
                    &input,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        let account = match outcome {
            IdempotentOutcome::Applied(account) => account,
            IdempotentOutcome::Replayed(_) => self
                .service
                .user(principal_id)
                .await?
                .ok_or(AuthorizationStateError::PrincipalMissing)?,
        };
        Ok(json!({ "user": user_value(account) }))
    }

    async fn require_admin_for_admin_target(
        &self,
        caller: &ValidatedRequest,
        principal_id: &str,
    ) -> Result<bool, AuthorizationStateError> {
        let admin_target = self
            .service
            .repository()
            .user_is_admin(principal_id.to_owned())
            .await?;
        if admin_target && !caller_is_admin(caller) {
            require_admin(caller)?;
        }
        Ok(admin_target)
    }
}

fn encode_generated_scalars(value: &mut Value) {
    match value {
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            *value = Value::String(number.to_string());
        }
        Value::Array(values) => values.iter_mut().for_each(encode_generated_scalars),
        Value::Object(values) => {
            for value in values.values_mut() {
                encode_generated_scalars(value);
            }
            if values.contains_key("action") {
                if let Some(target) = values.get_mut("target") {
                    encode_generated_bytes(target);
                }
            }
            if let Some(review_mode) = values.get_mut("reviewMode") {
                if !review_mode.is_null() {
                    encode_generated_bytes(review_mode);
                }
            }
        }
        _ => {}
    }
}

fn rpc_subject_prefix(api_id: &str) -> Result<String, AuthorizationStateError> {
    Ok(
        trellis_protocol::derive_bound_rpc_subject(api_id, "dep_trellis_auth_runtime", "Route")
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?
            .strip_suffix("Route")
            .expect("derived subject contains action")
            .to_owned(),
    )
}

fn resource_binding_value(
    evidence: ResourceBindingEvidence,
) -> Result<Value, AuthorizationStateError> {
    let mut value = serde_json::to_value(&evidence)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    let provider = value.get_mut("providerIdentity").ok_or_else(|| {
        AuthorizationStateError::Storage("resource binding lacks providerIdentity".to_owned())
    })?;
    encode_generated_bytes(provider);
    Ok(value)
}

fn encode_generated_bytes(value: &mut Value) {
    use base64::Engine as _;

    *value = Value::String(
        base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(value).expect("JSON value")),
    );
}

fn connection_value(connection: AuthConnectionPresence) -> Value {
    json!({
        "connectionId": connection.connection_id,
        "runtimeConnectionId": connection.runtime_connection_id,
        "loginSessionId": connection.login_session_id,
        "contextDigest": connection.context_digest,
        "principalId": connection.principal_id,
        "participantId": connection.participant_id,
        "deploymentId": connection.deployment_id,
        "instanceId": connection.instance_id,
        "serverId": connection.server_id,
        "clientId": connection.client_id,
        "userNkey": connection.user_nkey,
        "remoteAddress": connection.remote_address,
        "connectedAt": connection.connected_at,
        "lastSeenAt": connection.last_seen_at,
    })
}

fn service_instance_value(
    instance: super::RuntimeInstanceRecord,
    identity: ProvisionedIdentityRecord,
    profile: &DeploymentProfileRecord,
) -> Value {
    json!({
        "instanceId": instance.instance_id,
        "deploymentId": instance.deployment_id,
        "principalId": instance.principal_id,
        "identityPublicKey": identity.identity_public_key,
        "identityKeyId": identity.identity_key_id,
        "participantId": profile.participant_id,
        "state": instance.state,
        "createdAt": instance.created_at,
        "updatedAt": instance.updated_at,
        "version": instance.version,
    })
}

fn activation_review_value(review: DeviceActivationReviewRecord) -> Value {
    json!({
        "reviewId": review.review_id,
        "deploymentId": review.deployment_id,
        "instanceId": review.instance_id,
        "devicePrincipalId": review.principal_id,
        "activatedByUserPrincipalId": review.activated_by_user_principal_id,
        "state": review.state,
        "confirmationCode": review.payload.get("confirmationCode"),
        "requestedAt": review.requested_at,
        "expiresAt": review.expires_at,
        "decidedAt": review.decided_at,
        "decidedBy": review.decided_by,
        "reason": review.reason,
        "version": review.version,
    })
}

fn enum_string(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn deployment_state_wire(state: DeploymentProfileState) -> &'static str {
    match state {
        DeploymentProfileState::Active => "active",
        DeploymentProfileState::Disabled => "disabled",
        DeploymentProfileState::Removed => "revoked",
    }
}

fn paginate_values(
    mut entries: Vec<Value>,
    input: &Value,
    endpoint: &str,
    key_paths: &[&str],
) -> Result<Value, AuthorizationStateError> {
    let limit = input
        .pointer("/page/limit")
        .and_then(Value::as_u64)
        .unwrap_or(50);
    if limit == 0 || limit > 200 {
        return Err(AuthorizationStateError::InvalidRecord(
            "invalid pagination limit".to_owned(),
        ));
    }
    let limit = limit as usize;
    let mut query = input.clone();
    query.as_object_mut().map(|query| query.remove("page"));
    let query_digest = trellis_protocol::pagination_query_digest(endpoint, &query)
        .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
    let after = input
        .pointer("/page/cursor")
        .and_then(Value::as_str)
        .map(|cursor| {
            trellis_protocol::decode_pagination_cursor::<Vec<String>>(cursor, &query_digest)
        })
        .transpose()
        .map_err(|_| AuthorizationStateError::InvalidRecord("invalid pagination".to_owned()))?;
    let key = |entry: &Value| {
        key_paths
            .iter()
            .map(|path| {
                entry
                    .pointer(path)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    entries.sort_by_key(&key);
    if let Some(after) = after {
        if after.len() != key_paths.len() || after.iter().any(String::is_empty) {
            return Err(AuthorizationStateError::InvalidRecord(
                "invalid pagination".to_owned(),
            ));
        }
        entries.retain(|entry| key(entry) > after);
    }
    let mut entries = entries.into_iter().take(limit + 1).collect::<Vec<_>>();
    let next_cursor = if entries.len() > limit {
        entries.pop();
        let after = entries.last().map(key).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("invalid pagination".to_owned())
        })?;
        Some(
            trellis_protocol::encode_pagination_cursor(&query_digest, &after).map_err(|_| {
                AuthorizationStateError::InvalidRecord("invalid pagination".to_owned())
            })?,
        )
    } else {
        None
    };
    let page = next_cursor.map_or_else(|| json!({}), |cursor| json!({"nextCursor": cursor}));
    Ok(json!({ "items": entries, "page": page }))
}

fn paginate_sessions(
    entries: Vec<SessionRecord>,
    input: &Value,
) -> Result<Value, AuthorizationStateError> {
    paginate_values(
        entries
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        input,
        "auth.Sessions.List",
        &["/sessionId"],
    )
}

fn require_admin(caller: &ValidatedRequest) -> Result<(), AuthorizationStateError> {
    if caller_is_admin(caller) {
        Ok(())
    } else {
        Err(AuthorizationStateError::NotAuthorized)
    }
}

fn caller_is_admin(caller: &ValidatedRequest) -> bool {
    caller
        .platform_privileges
        .contains(&trellis_protocol::PlatformPrivilege::Admin)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, AuthorizationStateError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AuthorizationStateError::InvalidRecord(format!("{key} is required")))
}

fn nullable_string(value: &Value, key: &str) -> Result<Option<String>, AuthorizationStateError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(AuthorizationStateError::InvalidRecord(format!(
            "{key} must be a string or null"
        ))),
    }
}

fn required_u64(value: &Value, key: &str) -> Result<u64, AuthorizationStateError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| AuthorizationStateError::InvalidRecord(format!("{key} is required")))
}

fn required_i64(value: &Value, key: &str) -> Result<i64, AuthorizationStateError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| AuthorizationStateError::InvalidRecord(format!("{key} is required")))
}

fn required_nullable_u64(value: &Value, key: &str) -> Result<Option<u64>, AuthorizationStateError> {
    match value.get(key) {
        None => Err(AuthorizationStateError::InvalidRecord(format!(
            "{key} is required"
        ))),
        Some(Value::Null) => Ok(None),
        Some(Value::String(_)) => required_u64(value, key).map(Some),
        Some(_) => Err(AuthorizationStateError::InvalidRecord(format!(
            "{key} must be a decimal string or null"
        ))),
    }
}

fn required_nullable_i64(value: &Value, key: &str) -> Result<Option<i64>, AuthorizationStateError> {
    match value.get(key) {
        None => Err(AuthorizationStateError::InvalidRecord(format!(
            "{key} is required"
        ))),
        Some(Value::Null) => Ok(None),
        Some(Value::String(_)) => required_i64(value, key).map(Some),
        Some(_) => Err(AuthorizationStateError::InvalidRecord(format!(
            "{key} must be a decimal string or null"
        ))),
    }
}

fn next_version(version: u64) -> Result<u64, AuthorizationStateError> {
    let next = version
        .checked_add(1)
        .ok_or_else(|| AuthorizationStateError::Storage("version overflow".to_owned()))?;
    if next == 0 {
        return Err(AuthorizationStateError::Storage(
            "version overflow".to_owned(),
        ));
    }
    Ok(next)
}

fn required_bool(value: &Value, key: &str) -> Result<bool, AuthorizationStateError> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| AuthorizationStateError::InvalidRecord(format!("{key} is required")))
}

fn optional_string_array(
    value: &Value,
    key: &str,
) -> Result<Option<Vec<String>>, AuthorizationStateError> {
    match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(format!("{key} must contain strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => Err(AuthorizationStateError::InvalidRecord(format!(
            "{key} must be an array or null"
        ))),
    }
}

fn required_string_array(value: &Value, key: &str) -> Result<Vec<String>, AuthorizationStateError> {
    optional_string_array(value, key)?
        .ok_or_else(|| AuthorizationStateError::InvalidRecord(format!("{key} is required")))
}

fn sort_and_validate_role_mappings(
    role_mappings: &mut [PortalRoleMapping],
) -> Result<(), AuthorizationStateError> {
    role_mappings.sort_by(|left, right| {
        (&left.provider_id, &left.role).cmp(&(&right.provider_id, &right.role))
    });
    if role_mappings
        .windows(2)
        .any(|pair| pair[0].provider_id == pair[1].provider_id && pair[0].role == pair[1].role)
    {
        return Err(AuthorizationStateError::InvalidRecord(
            "roleMappings contains a duplicate providerId and role".to_owned(),
        ));
    }
    Ok(())
}

fn login_settings_from_value(
    portal_id: &str,
    value: &Value,
    provider_ids: Vec<String>,
    now: i64,
    version: u64,
) -> Result<LoginSettingsRecord, AuthorizationStateError> {
    Ok(LoginSettingsRecord {
        portal_id: portal_id.to_owned(),
        default_provider_id: (provider_ids.len() == 1).then(|| provider_ids[0].clone()),
        local_login_enabled: required_bool(value, "localLogin")?,
        federated_registration_enabled: required_bool(value, "federatedRegistration")?,
        provider_selection_enabled: provider_ids.len() > 1,
        updated_at: now,
        version,
    })
}

fn login_settings_value(portal: &LoginPortalRecord, settings: &LoginSettingsRecord) -> Value {
    json!({
        "providers": portal.provider_ids,
        "localLogin": settings.local_login_enabled,
        "localRegistration": portal.local_registration_enabled,
        "federatedRegistration": settings.federated_registration_enabled,
    })
}

fn portal_value(portal: LoginPortalRecord, settings: LoginSettingsRecord) -> Value {
    json!({
        "portalId": portal.portal_id,
        "displayName": portal.display_name,
        "entryUrl": portal.entry_url,
        "builtIn": portal.builtin,
        "disabled": portal.disabled,
        "loginSettings": login_settings_value(&portal, &settings),
        "createdAt": portal.created_at,
        "updatedAt": portal.updated_at,
        "version": portal.version,
    })
}

/// Bind an Auth mutation's exact input to its caller, purpose, and retry key.
pub(in crate::platform::auth) fn rpc_idempotency(
    purpose: &str,
    signer_id: &str,
    request_id: &str,
    input: &Value,
    now: i64,
) -> Result<IdempotencyResultRecord, AuthorizationStateError> {
    Ok(IdempotencyResultRecord {
        scope_key: digest_parts(&[purpose, signer_id, request_id]),
        purpose: purpose.to_owned(),
        signer_id: signer_id.to_owned(),
        request_id: request_id.to_owned(),
        request_digest: trellis_protocol::digest_json(input)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        result: Value::Null,
        created_at: now,
        expires_at: now.saturating_add(86_400_000),
    })
}

fn principal_state(principal: &super::PrincipalRecord) -> &'static str {
    match principal.state {
        PrincipalState::Active => "active",
        PrincipalState::Disabled => "disabled",
        PrincipalState::Revoked => "revoked",
    }
}

fn user_value(account: UserAccount) -> Value {
    json!({
        "userId": account.principal.principal_id,
        "principalId": account.profile.principal_id,
        "state": principal_state(&account.principal),
        "name": account.profile.display_name,
        "email": account.profile.email,
        "image": account.profile.image_url,
        "createdAt": account.principal.created_at,
        "updatedAt": account.profile.updated_at,
        "disabledAt": account.principal.disabled_at,
        "revokedAt": account.principal.revoked_at,
        "version": account.principal.version,
    })
}

fn digest_parts(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u32).to_be_bytes());
        hash.update(part.as_bytes());
    }
    URL_SAFE_NO_PAD.encode(hash.finalize())
}

fn now_millis() -> Result<i64, AuthorizationStateError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| AuthorizationStateError::Storage("current time overflow".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_page_omits_exhausted_next_cursor() {
        let response = paginate_values(
            vec![json!({ "id": 1 })],
            &json!({"page": {"limit": 1}}),
            "test",
            &["/id"],
        )
        .unwrap();
        assert_eq!(response["items"], json!([{ "id": 1 }]));
        assert_eq!(response.pointer("/page/nextCursor"), None);
    }

    #[test]
    fn cursor_page_survives_preceding_insert_and_delete_and_rejects_query_reuse() {
        let first = paginate_values(
            vec![json!({"id":"b"}), json!({"id":"c"}), json!({"id":"d"})],
            &json!({"state":"active", "page":{"limit":2}}),
            "test.List",
            &["/id"],
        )
        .unwrap();
        let cursor = first
            .pointer("/page/nextCursor")
            .and_then(Value::as_str)
            .unwrap();
        let second = paginate_values(
            vec![json!({"id":"a"}), json!({"id":"c"}), json!({"id":"d"})],
            &json!({"state":"active", "page":{"limit":2, "cursor":cursor}}),
            "test.List",
            &["/id"],
        )
        .unwrap();
        assert_eq!(second["items"], json!([{"id":"d"}]));
        assert!(paginate_values(
            vec![json!({"id":"d"})],
            &json!({"state":"revoked", "page":{"cursor":cursor}}),
            "test.List",
            &["/id"],
        )
        .is_err());
    }

    #[test]
    fn users_cursor_binds_state_and_search_filters() {
        let query = json!({"state":"active", "search":"needle"});
        let digest = trellis_protocol::pagination_query_digest("auth.Users.List", &query).unwrap();
        let cursor = trellis_protocol::encode_pagination_cursor(
            &digest,
            &(1_700_000_000_000_i64, "usr_target".to_owned()),
        )
        .unwrap();
        let changed_digest = trellis_protocol::pagination_query_digest(
            "auth.Users.List",
            &json!({"state":"disabled", "search":"needle"}),
        )
        .unwrap();

        assert!(trellis_protocol::decode_pagination_cursor::<(i64, String)>(
            &cursor,
            &changed_digest,
        )
        .is_err());
        let changed_digest = trellis_protocol::pagination_query_digest(
            "auth.Users.List",
            &json!({"state":"active", "search":"haystack"}),
        )
        .unwrap();
        assert!(trellis_protocol::decode_pagination_cursor::<(i64, String)>(
            &cursor,
            &changed_digest,
        )
        .is_err());
    }

    #[test]
    fn generated_integer_inputs_use_decimal_strings() {
        let input = json!({ "positive": "42", "signed": "-42" });
        assert_eq!(required_u64(&input, "positive").unwrap(), 42);
        assert_eq!(required_i64(&input, "signed").unwrap(), -42);
        assert!(required_u64(&json!({ "value": 42 }), "value").is_err());
    }

    #[test]
    fn duplicate_portal_role_mapping_is_rejected() {
        let mut mappings = vec![
            PortalRoleMapping {
                provider_id: "oidc".to_owned(),
                role: "operator".to_owned(),
                direct_capabilities: vec!["example::read".to_owned()],
                capability_group_keys: Vec::new(),
            },
            PortalRoleMapping {
                provider_id: "oidc".to_owned(),
                role: "operator".to_owned(),
                direct_capabilities: vec!["example::write".to_owned()],
                capability_group_keys: Vec::new(),
            },
        ];
        assert!(matches!(
            sort_and_validate_role_mappings(&mut mappings),
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
    }

    #[test]
    fn api_domain_classifies_bound_auth_and_core_subjects() {
        let auth_prefix =
            super::rpc_subject_prefix(trellis_runtime_apis::apis::trellis_auth_v1::API_ID)
                .expect("auth prefix");
        let core_prefix =
            super::rpc_subject_prefix(trellis_runtime_apis::apis::trellis_core_v1::API_ID)
                .expect("core prefix");
        assert_ne!(auth_prefix, core_prefix);
        assert!(auth_prefix.starts_with("rpc.v1."));
        assert!(core_prefix.starts_with("rpc.v1."));

        // Mirror the production rule exactly: only the Core prefix selects
        // Core, everything else is Auth.
        let classify = |subject: &str| {
            if subject.starts_with(&core_prefix) {
                error::ApiDomain::Core
            } else {
                error::ApiDomain::Auth
            }
        };
        for action in [
            "Users.Create",
            "Users.PasswordReset.Create",
            "Grants.Set",
            "Sessions.Revoke",
            "Portals.Put",
            "CapabilityGroups.Put",
        ] {
            let subject = format!("{auth_prefix}{action}");
            assert_eq!(classify(&subject), error::ApiDomain::Auth, "{subject}");
        }
        for action in ["Resources.Inspect", "Resources.Query", "Delete"] {
            let subject = format!("{core_prefix}{action}");
            assert_eq!(classify(&subject), error::ApiDomain::Core, "{subject}");
        }
        assert_eq!(
            classify("rpc.v1.unknown.other.Action"),
            error::ApiDomain::Auth
        );
    }

    #[test]
    fn public_rpc_errors_never_serialize_internal_causes() {
        let secret = "postgres://admin:secret@internal/auth";
        let payload = public_rpc_error(
            error::ApiDomain::Auth,
            &AuthorizationStateError::Storage(secret.to_owned()),
        );
        let encoded = serde_json::to_string(&payload).unwrap();
        assert!(!encoded.contains(secret));
        assert_eq!(payload["type"], "trellis.auth@v1::UnexpectedError");
        assert_eq!(payload["code"], "internal_error");
        let invalid = public_rpc_error(
            error::ApiDomain::Auth,
            &AuthorizationStateError::InvalidRecord(secret.to_owned()),
        );
        assert_eq!(invalid["type"], "trellis.auth@v1::ValidationError");
        assert_eq!(invalid["code"], "invalid_request");
        assert!(!serde_json::to_string(&invalid).unwrap().contains(secret));
    }

    #[test]
    fn generated_bytes_are_padded_standard_base64() {
        let mut value = serde_json::json!({
            "action": "call",
            "target": { "kind": "apiSurface" },
            "reviewMode": "required"
        });

        super::encode_generated_scalars(&mut value);

        assert_eq!(value["target"], "eyJraW5kIjoiYXBpU3VyZmFjZSJ9");
        assert_eq!(value["reviewMode"], "InJlcXVpcmVkIg==");
    }

    #[test]
    fn resource_binding_provider_identity_is_encoded_once_as_bytes() {
        let evidence = ResourceBindingEvidence {
            resource_kind: "kv".to_owned(),
            local_name: "records".to_owned(),
            binding_id: "binding-records".to_owned(),
            owner_participant_id: "example.Provider".to_owned(),
            provider_identity: super::super::ResourceProviderIdentity::Kv {
                bucket: "bucket-records".to_owned(),
            },
            actual: None,
            state: super::super::ResourceBindingState::Available,
            materialized_at: 1,
            error: None,
        };

        let mut encoded = super::resource_binding_value(evidence).unwrap();
        assert_eq!(encoded["state"], "available");
        assert_eq!(encoded["localName"], "records");
        let bytes = encoded["providerIdentity"]
            .as_str()
            .expect("bytes string")
            .to_owned();
        let decoded: Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(&bytes)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded["kind"], "kv");
        assert_eq!(decoded["bucket"], "bucket-records");

        super::encode_generated_scalars(&mut encoded);
        assert_eq!(encoded["providerIdentity"], bytes);
        assert_eq!(encoded["materializedAt"], "1");
    }

    #[test]
    fn nullable_version_helpers_distinguish_missing_null_and_malformed() {
        assert_eq!(
            super::required_nullable_u64(&serde_json::json!({}), "expectedVersion"),
            Err(AuthorizationStateError::InvalidRecord(
                "expectedVersion is required".to_owned()
            ))
        );
        assert_eq!(
            super::required_nullable_u64(
                &serde_json::json!({ "expectedVersion": null }),
                "expectedVersion"
            ),
            Ok(None)
        );
        assert_eq!(
            super::required_nullable_u64(
                &serde_json::json!({ "expectedVersion": "7" }),
                "expectedVersion"
            ),
            Ok(Some(7))
        );
        for malformed in [
            serde_json::json!(7),
            serde_json::json!("seven"),
            serde_json::json!("7.5"),
        ] {
            assert!(matches!(
                super::required_nullable_u64(
                    &serde_json::json!({ "expectedVersion": malformed }),
                    "expectedVersion"
                ),
                Err(AuthorizationStateError::InvalidRecord(_))
            ));
        }
        assert_eq!(
            super::required_nullable_i64(&serde_json::json!({ "expiresAt": "-1" }), "expiresAt"),
            Ok(Some(-1))
        );
        assert!(matches!(
            super::required_nullable_i64(&serde_json::json!({ "expiresAt": 1 }), "expiresAt"),
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
        assert!(super::next_version(u64::MAX).is_err());
        assert_eq!(super::next_version(u64::MAX - 1).unwrap(), u64::MAX);
    }
}
