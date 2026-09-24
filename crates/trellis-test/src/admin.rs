//! Administrator automation through the projected generated Auth API.
//!
//! All administration uses the generated `trellis.auth@v1` client projected
//! into this crate; the only HTTP calls are the existing local-login and
//! first-administrator portal endpoints, which the SDK does not expose as RPCs.

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use trellis_rs::auth::{
    approve_local_login, begin_local_login, complete_local_login, connect_admin_client_async,
    start_agent_login, LocalLoginStep, StartAgentLoginOpts,
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
///
/// Uses the staged local-login path so a caller that needs a broader consent
/// ceiling gets the built-in portal policy configured before approval.
pub(crate) async fn login_client(
    trellis_url: &str,
    participant_id: &str,
    username: &str,
    password: &str,
    admin: &AdminSession,
) -> Result<ClientSession, TrellisTestError> {
    let challenge = start_agent_login(&StartAgentLoginOpts {
        trellis_url,
        participant_id,
        allow_insecure_origin: false,
    })
    .await
    .map_err(|error| login_error("starting the caller login", &error))?;
    let login_url = challenge.login_url().to_owned();
    match begin_local_login(trellis_url, &login_url, username, password)
        .await
        .map_err(|error| login_error("completing the caller login", &error))?
    {
        LocalLoginStep::Completed { .. } => {}
        LocalLoginStep::ConsentRequired {
            flow_id,
            binding,
            summary,
        } => {
            let consent_participant = summary.participant_id.as_deref().ok_or_else(|| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Authentication,
                    TrellisTestStage::ClientLogin,
                    "the caller consent view named no participant",
                )
            })?;
            if consent_participant != participant_id {
                return Err(TrellisTestError::new(
                    TrellisTestErrorKind::Authentication,
                    TrellisTestStage::ClientLogin,
                    format!(
                        "the caller consent view named participant '{consent_participant}' instead of '{participant_id}'"
                    ),
                ));
            }
            admin
                .ensure_portal_consent_policy(participant_id, &summary.capabilities)
                .await?;
            approve_local_login(trellis_url, &flow_id, &binding)
                .await
                .map_err(|error| login_error("approving the caller consent", &error))?;
        }
    }
    let state = challenge
        .complete_session(trellis_url)
        .await
        .map_err(|error| login_error("binding the caller session", &error))?;
    if state.participant_id != participant_id || state.trellis_url != trellis_url {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::Authentication,
            TrellisTestStage::ClientLogin,
            "the bound caller session did not match the requested participant and origin",
        ));
    }
    Ok(ClientSession {
        login_session_id: state.login_session_id,
        session_seed: state.session_seed,
    })
}

fn login_error(action: &str, error: &impl std::fmt::Display) -> TrellisTestError {
    TrellisTestError::new(
        TrellisTestErrorKind::Authentication,
        TrellisTestStage::ClientLogin,
        format!("{action}: {error}"),
    )
}

/// Redacts a known secret from a message before it can reach a public error.
fn redact(text: &str, secret: &str) -> String {
    crate::error::redact_secrets(text, &[secret])
}

/// An authenticated administrator session for one test runtime.
pub(crate) struct AdminSession {
    auth: AuthClient,
}

impl AdminSession {
    /// Completes the real local login as the sandbox administrator.
    pub(crate) async fn connect(
        trellis_url: &str,
        username: &str,
        password: &str,
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
        })
    }

    /// Confirms the administrator boundary with a generated `Sessions.Me` call.
    pub(crate) async fn verify(&self) -> Result<(), TrellisTestError> {
        let request = crate::types::AuthSessionsMeRequest {};
        let response = self.auth.sessions_me(&request).await.map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::AdministratorLogin,
                format!("verifying the administrator session: {error}"),
            )
        })?;
        let participant = value_text(&response.connection.participant_id);
        if participant != crate::participants::trellis_cli::PARTICIPANT_ID {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::AdministratorLogin,
                format!(
                    "the administrator connection named participant '{participant}' instead of the built-in CLI participant"
                ),
            ));
        }
        match &response.session {
            crate::__types::Nullable::Value(session) => {
                let bound = value_text(&session.participant_id);
                if bound != participant {
                    return Err(TrellisTestError::new(
                        TrellisTestErrorKind::Authentication,
                        TrellisTestStage::AdministratorLogin,
                        "the administrator login session was bound to a different participant",
                    ));
                }
            }
            crate::__types::Nullable::Null => {
                return Err(TrellisTestError::new(
                    TrellisTestErrorKind::Authentication,
                    TrellisTestStage::AdministratorLogin,
                    "the administrator connection carried no bound login session",
                ));
            }
        }
        Ok(())
    }

    /// Installs `P` at revision 0, returning the server-confirmed revision.
    pub(crate) async fn install_participant<P: ParticipantDescriptor>(
        &self,
    ) -> Result<u64, TrellisTestError> {
        let evidence = P::package_evidence();
        let digest = evidence.root_digest().to_owned();
        let response = self
            .auth
            .participants_install(&AuthParticipantsInstallRequest {
                expected_revision: wire("0")?,
                idempotency_key: wire(idempotency_key())?,
                package_digest: digest.clone(),
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
        let installed = &response.participant;
        if value_text(&installed.participant_id) != P::ID || installed.package_digest != digest {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::AdminRpc,
                TrellisTestStage::ParticipantInstallation,
                format!(
                    "the install response did not confirm participant {} at the requested digest",
                    P::ID
                ),
            ));
        }
        let revision = value_text(&installed.revision)
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
                request.approval = Some(approve_required(&consent)?);
                // A changed consent request is a new operation and must not
                // replay the first attempt's idempotency key.
                request.idempotency_key = wire(idempotency_key())?;
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

    /// Provisions a server-assigned service instance for the exact participant.
    pub(crate) async fn provision_service_instance(
        &self,
        deployment_id: &str,
        participant_id: &str,
    ) -> Result<(String, String), TrellisTestError> {
        let (seed, key) = trellis_rs::auth::generate_session_keypair();
        let instance = self
            .auth
            .service_instances_provision(&AuthServiceInstancesProvisionRequest {
                deployment_id: wire(deployment_id)?,
                instance_id: wire(None::<String>)?,
                identity_public_key: wire(key)?,
                participant_id: wire(Some(participant_id.to_owned()))?,
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
        if let crate::__types::Nullable::Value(assigned) = &instance.participant_id {
            if assigned != participant_id {
                return Err(TrellisTestError::new(
                    TrellisTestErrorKind::AdminRpc,
                    TrellisTestStage::ServiceProvisioning,
                    "the provisioned instance was not assigned to the requested participant",
                ));
            }
        }
        let instance_id = value_text(&instance.instance_id);
        if instance_id.is_empty() {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::AdminRpc,
                TrellisTestStage::ServiceProvisioning,
                "the provisioned instance carried no server-assigned id",
            ));
        }
        Ok((instance_id, seed))
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
                format!(
                    "posting the first-administrator credentials: {}",
                    redact(&error.to_string(), token)
                ),
            )
        })?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::AdministratorBootstrap,
            format!(
                "reading the first-administrator response: {}",
                redact(&error.to_string(), token)
            ),
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
///
/// A required item the server marked ineligible fails registration rather than
/// silently submitting a partial approval.
fn approve_required(consent: &ConsentRequest) -> Result<Approval, TrellisTestError> {
    if let Some(item) = consent
        .capabilities
        .iter()
        .find(|capability| capability.required && !capability.eligible)
    {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::AdminRpc,
            TrellisTestStage::DeploymentCreateApply,
            format!(
                "required capability '{}' is not eligible for consent",
                item.id
            ),
        ));
    }
    if let Some(item) = consent
        .resources
        .iter()
        .find(|resource| resource.required && !resource.eligible)
    {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::AdminRpc,
            TrellisTestStage::DeploymentCreateApply,
            format!(
                "required resource '{}' is not eligible for consent",
                item.name
            ),
        ));
    }
    Ok(Approval {
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
    })
}
