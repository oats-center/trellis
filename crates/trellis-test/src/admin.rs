//! Administrator automation: complete the real local login and drive admin RPCs.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use trellis_rs::auth::{
    complete_local_login, connect_admin_client_async, start_agent_login, StartAgentLoginOpts,
};
use trellis_rs::client::CallError;
use trellis_runtime_apis::apis::trellis_auth_v1::rpc::{
    DeploymentsApplyError, ParticipantsGetError as AuthParticipantsGetError,
};
use trellis_runtime_apis::apis::trellis_auth_v1::Client as AuthClient;
use trellis_runtime_apis::types::{
    Approval, ApprovalMode, ApprovedCapability, ApprovedResource, AuthDeploymentsApplyRequest,
    AuthDeploymentsCreateRequest, AuthDeploymentsCreateRequestKind, AuthParticipantsGetRequest,
    AuthParticipantsInstallRequest, AuthPortalsGrantOverridesPutRequest,
    AuthServiceInstancesProvisionRequest, ConsentRequest,
};

use crate::error::TrellisTestError;
use crate::runtime::TrellisTestRuntime;

fn wire<T: serde::de::DeserializeOwned>(value: impl Serialize) -> Result<T, TrellisTestError> {
    let value =
        serde_json::to_value(value).map_err(|error| TrellisTestError::Config(error.to_string()))?;
    serde_json::from_value(value).map_err(|error| TrellisTestError::Config(error.to_string()))
}

fn idempotency_key() -> String {
    ulid::Ulid::new().to_string()
}

/// A provisioned service instance and the seed its service must present to connect.
pub struct TrellisTestServiceInstance {
    /// Instance id assigned by the platform.
    pub instance_id: String,
    /// Session seed the service reads from `TRELLIS_SEED`.
    pub seed: String,
}

/// Connection material for a provisioned caller (client) participant.
///
/// The harness cannot produce a typed client — that type belongs to the caller's own generated
/// contract — so it hands back exactly what a client needs to connect:
/// `ServiceConnectOptions::new(&session.trellis_url, &session.seed)`.
pub struct TrellisTestClientSession {
    /// Control-plane base URL.
    pub trellis_url: String,
    /// Session seed for the provisioned instance.
    pub seed: String,
    /// Instance id assigned by the platform.
    pub instance_id: String,
    /// Participant identity that owns the instance.
    pub participant_id: String,
}

/// An authenticated administrator client for a test runtime.
pub struct TrellisTestAdmin {
    auth: AuthClient,
    trellis_url: String,
    deployment: String,
}

impl TrellisTestRuntime {
    /// Completes the real local login as the seeded administrator and returns an admin client.
    ///
    /// The login runs the ordinary detached-agent flow — start, local password, consent approval —
    /// and binds the session **without persisting it**, so a test never writes the machine's
    /// session store.
    ///
    /// # Errors
    ///
    /// Returns an error when the login flow is denied or the resulting session is not an
    /// administrator.
    pub async fn connect_admin(&self) -> Result<TrellisTestAdmin, TrellisTestError> {
        let challenge = start_agent_login(&StartAgentLoginOpts {
            trellis_url: self.trellis_url(),
            participant_id: trellis_runtime_apis::participants::trellis_cli::PARTICIPANT_ID,
            allow_insecure_origin: false,
        })
        .await
        .map_err(|error| TrellisTestError::Runtime(error.to_string()))?;
        let login_url = challenge.login_url().to_owned();
        complete_local_login(
            self.trellis_url(),
            &login_url,
            self.admin_username(),
            self.admin_password(),
        )
        .await
        .map_err(|error| TrellisTestError::Runtime(format!("local login failed: {error}")))?;
        let outcome = challenge
            .complete_without_persistence(self.trellis_url())
            .await
            .map_err(|error| {
                TrellisTestError::Runtime(format!("login completion failed: {error}"))
            })?;
        let generated = connect_admin_client_async(&outcome.state)
            .await
            .map_err(|error| TrellisTestError::Runtime(error.to_string()))?;
        Ok(TrellisTestAdmin {
            auth: AuthClient::from_generated(generated),
            trellis_url: self.trellis_url().to_owned(),
            deployment: self.deployment().to_owned(),
        })
    }
}

impl TrellisTestAdmin {
    /// Default deployment name this admin client was created for.
    #[must_use]
    pub fn deployment(&self) -> &str {
        &self.deployment
    }

    /// Creates a service deployment with `display_name` and returns its deployment id.
    ///
    /// # Errors
    ///
    /// Returns an error when the deployment cannot be created.
    pub async fn create_deployment(&self, display_name: &str) -> Result<String, TrellisTestError> {
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
            .map_err(|error| TrellisTestError::Runtime(error.to_string()))?;
        Ok(value_text(&response.deployment.deployment_id))
    }
}

impl TrellisTestAdmin {
    /// Compiles `source` and applies the selected participant to `deployment_id`, approving the
    /// server-computed consent request exactly as `trellis svc apply --yes` does.
    ///
    /// # Errors
    ///
    /// Returns an error when the participant cannot be compiled, the apply is rejected, or the
    /// re-apply after consent still fails.
    pub async fn apply_participant(
        &self,
        deployment_id: &str,
        source: &Path,
        participant_id: Option<&str>,
    ) -> Result<(), TrellisTestError> {
        self.install_participant(source, participant_id).await?;
        let participant = trellis_cli::app::deploy::compile_participant_input(
            source,
            participant_id,
            None,
            &[],
            &[],
        )
        .map_err(|error| TrellisTestError::Config(format!("compiling participant: {error}")))?;
        // A freshly created deployment applies at revision 0; the list entry's `version` field is
        // the deployment's own version, not the revision `Deployments.Apply` expects.
        let mut request = AuthDeploymentsApplyRequest {
            expected_revision: wire("0")?,
            approval: None,
            deployment_id: wire(deployment_id)?,
            idempotency_key: wire(idempotency_key())?,
            package_evidence: participant.package_evidence,
            participant_path: participant.participant_path,
            package_digest: participant.package_digest,
        };
        match self.auth.deployments_apply(&request).await {
            Ok(_) => Ok(()),
            Err(error) => {
                let consent = consent_request(&error).ok_or_else(|| {
                    TrellisTestError::Runtime(format!("applying participant failed: {error}"))
                })?;
                request.approval = Some(approve_required(&consent));
                self.auth
                    .deployments_apply(&request)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        TrellisTestError::Runtime(format!(
                            "applying participant after consent failed: {error}"
                        ))
                    })
            }
        }
    }
}

impl TrellisTestAdmin {
    /// Provisions a service instance for `deployment_id` and returns its id and session seed.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform rejects the provisioning.
    pub async fn provision_service_instance(
        &self,
        deployment_id: &str,
    ) -> Result<TrellisTestServiceInstance, TrellisTestError> {
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
            .map_err(|error| TrellisTestError::Runtime(error.to_string()))?
            .instance;
        Ok(TrellisTestServiceInstance {
            instance_id: value_text(&instance.instance_id),
            seed,
        })
    }
}

impl TrellisTestAdmin {
    /// Creates `display_name` as a deployment of `source`'s participant and provisions one
    /// instance for it, returning the material a caller connects with.
    ///
    /// # Errors
    ///
    /// Returns an error when the participant cannot be compiled, applied, or provisioned.
    pub async fn register_client(
        &self,
        display_name: &str,
        source: &Path,
        participant_id: Option<&str>,
    ) -> Result<TrellisTestClientSession, TrellisTestError> {
        let deployment_id = self.create_deployment(display_name).await?;
        self.apply_participant(&deployment_id, source, participant_id)
            .await?;
        let instance = self.provision_service_instance(&deployment_id).await?;
        Ok(TrellisTestClientSession {
            trellis_url: self.trellis_url.clone(),
            seed: instance.seed,
            instance_id: instance.instance_id,
            participant_id: participant_id.unwrap_or_default().to_owned(),
        })
    }
}

impl TrellisTestAdmin {
    /// Installs (or updates the definition of) `source`'s participant, as `trellis participants
    /// install` does. A participant must be installed before a deployment can apply it.
    ///
    /// # Errors
    ///
    /// Returns an error when the participant cannot be compiled or the install is rejected.
    pub async fn install_participant(
        &self,
        source: &Path,
        participant_id: Option<&str>,
    ) -> Result<(), TrellisTestError> {
        let participant = trellis_cli::app::deploy::compile_participant_input(
            source,
            participant_id,
            None,
            &[],
            &[],
        )
        .map_err(|error| TrellisTestError::Config(format!("compiling participant: {error}")))?;
        let expected_revision = match self
            .auth
            .participants_get(&AuthParticipantsGetRequest {
                participant_id: wire(&participant.participant_id)?,
                revision: None,
            })
            .await
        {
            Ok(current) => current
                .participant
                .revision
                .to_string()
                .trim_matches('"')
                .to_owned(),
            Err(CallError::Declared(error))
                if matches!(
                    error.as_ref(),
                    AuthParticipantsGetError::AuthError(error)
                        if error.payload().is_ok_and(|details| details.code.as_ref() == "not_found")
                ) =>
            {
                "0".to_owned()
            }
            Err(error) => {
                return Err(TrellisTestError::Runtime(format!(
                    "reading participant revision: {error}"
                )));
            }
        };
        self.auth
            .participants_install(&AuthParticipantsInstallRequest {
                expected_revision: wire(expected_revision)?,
                idempotency_key: wire(idempotency_key())?,
                package_digest: wire(participant.package_digest)?,
                package_evidence: participant.package_evidence,
                participant_path: wire(participant.participant_path)?,
                platform_trust: Some(false),
            })
            .await
            .map_err(|error| {
                TrellisTestError::Runtime(format!("installing participant: {error}"))
            })?;
        Ok(())
    }
}

impl TrellisTestAdmin {
    /// Configures the built-in portal's consent ceiling for `participant_id`, so a browser app
    /// participant can be consented to during its portal login.
    ///
    /// Mirrors the TypeScript harness's `ensurePortalConsentPolicy`.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform rejects the override.
    pub async fn ensure_portal_consent_policy(
        &self,
        participant_id: &str,
        capability_ids: &[String],
    ) -> Result<(), TrellisTestError> {
        let request: AuthPortalsGrantOverridesPutRequest = wire(serde_json::json!({
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
                TrellisTestError::Runtime(format!("setting portal consent policy: {error}"))
            })?;
        Ok(())
    }
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

/// Renders a generated newtype string wrapper without depending on its concrete shape.
fn value_text(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}
