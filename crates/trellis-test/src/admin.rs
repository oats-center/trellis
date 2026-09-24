//! Administrator automation through the projected generated Auth API.
//!
//! All administration uses the generated `trellis.auth@v1` client projected
//! into this crate; the only HTTP calls are the existing local-login and
//! first-administrator portal endpoints, which the SDK does not expose as RPCs.

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use trellis_rs::auth::{
    complete_local_login, connect_admin_client_async, start_agent_login, StartAgentLoginOpts,
};
use trellis_rs::client::CallError;
use trellis_rs::generated::ParticipantDescriptor;

use crate::apis::trellis_auth_v1::rpc::DeploymentsApplyError;
use crate::apis::trellis_auth_v1::Client as AuthClient;
use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use crate::types::{
    Approval, ApprovalMode, ApprovedCapability, ApprovedResource, AuthDeploymentsApplyRequest,
    AuthDeploymentsCreateRequest, AuthDeploymentsCreateRequestKind, AuthParticipantsInstallRequest,
    AuthPortalsGrantOverridesPutRequest, AuthServiceInstancesProvisionRequest, ConsentRequest,
};

/// Fixed local username for the isolated sandbox administrator.
pub(crate) const ADMIN_USERNAME: &str = "trellis-test-admin";

fn wire<T: serde::de::DeserializeOwned>(value: impl Serialize) -> Result<T, TrellisTestError> {
    let value = serde_json::to_value(value).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::InvalidConfiguration,
            TrellisTestStage::ParticipantInstallation,
            error.to_string(),
        )
    })?;
    serde_json::from_value(value).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::InvalidConfiguration,
            TrellisTestStage::ParticipantInstallation,
            error.to_string(),
        )
    })
}

fn idempotency_key() -> String {
    ulid::Ulid::new().to_string()
}

/// Renders a generated newtype string wrapper without depending on its shape.
fn value_text(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// A logged-in client session's connection material.
pub(crate) struct ClientSession {
    pub(crate) login_session_id: String,
    pub(crate) session_seed: String,
}

/// Completes a fresh participant-bound login for an app/agent caller.
pub(crate) async fn login_client(
    trellis_url: &str,
    participant_id: &str,
    username: &str,
    password: &str,
    timeout_ms: u64,
) -> Result<ClientSession, TrellisTestError> {
    let challenge = start_agent_login(&StartAgentLoginOpts {
        trellis_url,
        participant_id,
        allow_insecure_origin: false,
    })
    .await
    .map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Authentication,
            TrellisTestStage::ClientLogin,
            format!("starting the caller login: {error}"),
        )
    })?;
    let login_url = challenge.login_url().to_owned();
    complete_local_login(trellis_url, &login_url, username, password)
        .await
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::ClientLogin,
                format!("completing the caller login: {error}"),
            )
        })?;
    let state = challenge
        .complete_session(trellis_url)
        .await
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::ClientLogin,
                format!("binding the caller session: {error}"),
            )
        })?;
    let _ = timeout_ms;
    Ok(ClientSession {
        login_session_id: state.login_session_id,
        session_seed: state.session_seed,
    })
}

/// An authenticated administrator session for one test runtime.
pub(crate) struct AdminSession {
    auth: AuthClient,
    pub(crate) trellis_url: String,
    pub(crate) timeout_ms: u64,
}

impl AdminSession {
    /// Completes the real local login as the sandbox administrator.
    pub(crate) async fn connect(
        trellis_url: &str,
        username: &str,
        password: &str,
        timeout_ms: u64,
    ) -> Result<Self, TrellisTestError> {
        let challenge = start_agent_login(&StartAgentLoginOpts {
            trellis_url,
            participant_id: crate::participants::trellis_cli::PARTICIPANT_ID,
            allow_insecure_origin: false,
        })
        .await
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::AdministratorLogin,
                format!("starting the administrator login: {error}"),
            )
        })?;
        let login_url = challenge.login_url().to_owned();
        complete_local_login(trellis_url, &login_url, username, password)
            .await
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Authentication,
                    TrellisTestStage::AdministratorLogin,
                    format!("completing the local login: {error}"),
                )
            })?;
        let state = challenge
            .complete_session(trellis_url)
            .await
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Authentication,
                    TrellisTestStage::AdministratorLogin,
                    format!("binding the administrator session: {error}"),
                )
            })?;
        let generated = connect_admin_client_async(&state).await.map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::AdministratorLogin,
                format!("connecting the administrator client: {error}"),
            )
        })?;
        Ok(Self {
            auth: AuthClient::from_generated(generated),
            trellis_url: trellis_url.to_owned(),
            timeout_ms,
        })
    }

    /// Confirms the administrator boundary with a generated `Sessions.Me` call.
    pub(crate) async fn verify(&self) -> Result<(), TrellisTestError> {
        let request = crate::types::AuthSessionsMeRequest {};
        self.auth
            .sessions_me(&request)
            .await
            .map(|_| ())
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Authentication,
                    TrellisTestStage::AdministratorLogin,
                    format!("verifying the administrator session: {error}"),
                )
            })
    }

    /// Installs `P` at revision 0, returning the server-confirmed revision.
    pub(crate) async fn install_participant<P: ParticipantDescriptor>(
        &self,
    ) -> Result<u64, TrellisTestError> {
        let evidence = P::package_evidence();
        let response = self
            .auth
            .participants_install(&AuthParticipantsInstallRequest {
                expected_revision: wire("0")?,
                idempotency_key: wire(idempotency_key())?,
                package_digest: evidence.root_digest().to_owned(),
                package_evidence: wire(evidence)?,
                participant_path: P::PATH.to_owned(),
                platform_trust: Some(false),
            })
            .await
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::AdminRpc,
                    TrellisTestStage::ParticipantInstallation,
                    format!("installing participant {}: {error}", P::ID),
                )
            })?;
        let revision = value_text(&response.participant.revision)
            .parse::<u64>()
            .map_err(|_| {
                TrellisTestError::new(
                    TrellisTestErrorKind::AdminRpc,
                    TrellisTestStage::ParticipantInstallation,
                    "the install response carried an invalid revision",
                )
            })?;
        Ok(revision)
    }

    /// Creates a service deployment named `display_name` and returns its id.
    pub(crate) async fn create_deployment(
        &self,
        display_name: &str,
    ) -> Result<String, TrellisTestError> {
        let response = self
            .auth
            .deployments_create(&AuthDeploymentsCreateRequest {
                kind: AuthDeploymentsCreateRequestKind::Service,
                display_name: wire(display_name)?,
                participant_id: wire(None::<String>)?,
                expires_at: wire(None::<String>)?,
                requires_device_delegation: false,
                review_mode: wire(None::<String>)?,
                portal_id: wire(None::<String>)?,
                idempotency_key: wire(idempotency_key())?,
            })
            .await
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::AdminRpc,
                    TrellisTestStage::DeploymentCreateApply,
                    format!("creating deployment '{display_name}': {error}"),
                )
            })?;
        Ok(value_text(&response.deployment.deployment_id))
    }

    /// Applies `P` to `deployment_id`, approving the server-computed consent.
    pub(crate) async fn apply_participant<P: ParticipantDescriptor>(
        &self,
        deployment_id: &str,
    ) -> Result<(), TrellisTestError> {
        let evidence = P::package_evidence();
        let mut request = AuthDeploymentsApplyRequest {
            approval: None,
            deployment_id: wire(deployment_id)?,
            expected_revision: wire("0")?,
            idempotency_key: wire(idempotency_key())?,
            package_digest: evidence.root_digest().to_owned(),
            package_evidence: wire(evidence)?,
            participant_path: P::PATH.to_owned(),
        };
        match self.auth.deployments_apply(&request).await {
            Ok(_) => Ok(()),
            Err(error) => {
                let consent = consent_request(&error).ok_or_else(|| {
                    TrellisTestError::new(
                        TrellisTestErrorKind::AdminRpc,
                        TrellisTestStage::DeploymentCreateApply,
                        format!("applying participant {}: {error}", P::ID),
                    )
                })?;
                request.approval = Some(approve_required(&consent));
                self.auth
                    .deployments_apply(&request)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        TrellisTestError::new(
                            TrellisTestErrorKind::AdminRpc,
                            TrellisTestStage::DeploymentCreateApply,
                            format!("applying participant {} after consent: {error}", P::ID),
                        )
                    })
            }
        }
    }

    /// Provisions a service instance and returns its id and identity seed.
    pub(crate) async fn provision_service_instance(
        &self,
        deployment_id: &str,
    ) -> Result<(String, String), TrellisTestError> {
        let (seed, key) = trellis_rs::auth::generate_session_keypair();
        let instance = self
            .auth
            .service_instances_provision(&AuthServiceInstancesProvisionRequest {
                deployment_id: wire(deployment_id)?,
                instance_id: wire(Some(format!("inst_{}", &key[..16])))?,
                identity_public_key: wire(key)?,
                participant_id: wire(None::<String>)?,
                idempotency_key: wire(idempotency_key())?,
            })
            .await
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::AdminRpc,
                    TrellisTestStage::ServiceProvisioning,
                    format!("provisioning a service instance: {error}"),
                )
            })?
            .instance;
        Ok((value_text(&instance.instance_id), seed))
    }

    /// Sets the built-in portal's consent ceiling for `participant_id`.
    pub(crate) async fn ensure_portal_consent_policy(
        &self,
        participant_id: &str,
        capability_ids: &[String],
    ) -> Result<(), TrellisTestError> {
        let request: AuthPortalsGrantOverridesPutRequest = wire(json!({
            "portalId": "builtin",
            "participantId": participant_id,
            "directCapabilities": capability_ids,
            "capabilityGroupKeys": [],
            "roleMappings": [],
            "expectedVersion": null,
            "idempotencyKey": idempotency_key(),
        }))?;
        self.auth
            .portals_grant_overrides_put(&request)
            .await
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::AdminRpc,
                    TrellisTestStage::ClientLogin,
                    format!("setting the portal consent policy: {error}"),
                )
            })?;
        Ok(())
    }
}

/// Completes the first-administrator flow for a bootstrap `token`.
pub(crate) async fn bootstrap_first_admin(
    trellis_url: &str,
    token: &str,
    username: &str,
    password: &str,
    timeout_ms: u64,
) -> Result<(), TrellisTestError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::AdministratorBootstrap,
                format!("building the bootstrap HTTP client: {error}"),
            )
        })?;
    let url = format!(
        "{}/auth/account-flow/{}/local-password",
        trellis_url.trim_end_matches('/'),
        token
    );
    let response = client
        .post(&url)
        .header("origin", trellis_url)
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::AdministratorBootstrap,
                format!("posting the first-administrator credentials: {error}"),
            )
        })?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::AdministratorBootstrap,
            format!("reading the first-administrator response: {error}"),
        )
    })?;
    if !status.is_success() || body.get("status").and_then(Value::as_str) != Some("created") {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::AdministratorBootstrap,
            format!("first-administrator setup failed with status {status}"),
        ));
    }
    Ok(())
}

/// Extracts the server-computed consent request from an apply rejection.
fn consent_request(error: &CallError<DeploymentsApplyError>) -> Option<ConsentRequest> {
    let CallError::Declared(error) = error else {
        return None;
    };
    let DeploymentsApplyError::AuthError(error) = error.as_ref() else {
        return None;
    };
    serde_json::from_value(error.error.extra.get("consentRequest")?.clone()).ok()
}

/// Approves every eligible capability and resource, matching `trellis svc apply --yes`.
fn approve_required(consent: &ConsentRequest) -> Approval {
    Approval {
        approved_capabilities: consent
            .capabilities
            .iter()
            .filter(|capability| capability.eligible)
            .map(|capability| ApprovedCapability {
                id: capability.id.clone(),
                consent_digest: capability.consent_digest.clone(),
            })
            .collect(),
        approved_resources: consent
            .resources
            .iter()
            .filter(|resource| resource.eligible)
            .map(|resource| ApprovedResource {
                kind: resource.kind.clone(),
                name: resource.name.clone(),
                commitment: resource.requested_commitment.clone(),
            })
            .collect(),
        companion_approved: consent.companion.is_some(),
        decision_digest: consent.decision_digest.clone(),
        delegation_ceiling: None,
        expected_grant_revision: consent.expected_grant_revision,
        installed_revision: consent.installed_revision,
        mode: ApprovalMode::Capabilities,
    }
}
