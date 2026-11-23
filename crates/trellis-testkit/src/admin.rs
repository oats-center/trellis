//! Automation over ordinary native OAuth, portal HTTP, and generated Auth RPCs.

use std::path::Path;

use crate::apis::trellis_auth_v1::rpc::ApisGetError;
use crate::apis::trellis_auth_v1::Client as AuthClient;
use crate::types::{
    AuthApiAcceptRequest, AuthApiGetRequest, AuthApiReviewRequest, AuthCapabilityGrantRequest,
    AuthCapabilityGrantsListRequest, AuthDeploymentCreateRequest, AuthDeploymentKind, AuthEmpty,
    AuthInstanceProvisionRequest, AuthOAuthClientPutRequest, AuthPublicClientKind,
    AuthUserCreateRequest,
};
use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use serde::Serialize;
use serde_json::{json, Value};
use trellis_rs::auth::{
    discover_native_oauth, NativeCredentialScope, NativeOAuthClient, NativeOAuthSession,
};
use trellis_rs::client::CallError;
use trellis_rs::client::{RuntimeAuthorizationTrust, UserConnectOptions};
use trellis_rs::generated::{AppRequestDescriptor, ParticipantDescriptor};

use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};

pub(crate) const ADMIN_USERNAME: &str = "trellis-testkit-admin";

fn failure(stage: TrellisTestStage, error: impl std::fmt::Display) -> TrellisTestError {
    let kind = match stage {
        TrellisTestStage::ClientLogin | TrellisTestStage::AdministratorLogin => {
            TrellisTestErrorKind::Authentication
        }
        _ => TrellisTestErrorKind::AdminRpc,
    };
    TrellisTestError::new(kind, stage, error.to_string())
}

fn wire<T: serde::de::DeserializeOwned>(value: impl Serialize) -> Result<T, TrellisTestError> {
    serde_json::from_value(
        serde_json::to_value(value)
            .map_err(|error| failure(TrellisTestStage::ParticipantInstallation, error))?,
    )
    .map_err(|error| failure(TrellisTestStage::ParticipantInstallation, error))
}

/// Complete the public native Device Code flow with an actual portal session,
/// login, consent presentation, and explicit approval. No cookies or authority
/// are injected into the runtime.
pub(crate) async fn login_client(
    oauth_origin: &str,
    trust_roots: &[std::path::PathBuf],
    custody: &Path,
    client_id: &str,
    details: &Value,
    username: &str,
    password: &str,
) -> Result<(NativeOAuthSession, Vec<String>), TrellisTestError> {
    let stage = TrellisTestStage::ClientLogin;
    let server = discover_native_oauth(oauth_origin, trust_roots)
        .await
        .map_err(|error| failure(stage, format!("discovering native OAuth: {error}")))?;
    let native = NativeOAuthClient::open(
        custody.to_owned(),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: client_id.into(),
            credential_handle: ulid::Ulid::generate(),
        },
        server.trust,
    )
    .await
    .map_err(|error| failure(stage, format!("opening native OAuth custody: {error}")))?;
    let device = native
        .start_device(details)
        .await
        .map_err(|error| failure(stage, format!("starting native Device Code: {error}")))?;
    approve_device(
        oauth_origin,
        trust_roots,
        client_id,
        device.user_code(),
        username,
        password,
        false,
    )
    .await?;
    let session = device
        .complete()
        .await
        .map_err(|error| failure(stage, error))?;
    Ok((session, server.nats_servers))
}

pub(crate) async fn approve_device(
    oauth_origin: &str,
    trust_roots: &[std::path::PathBuf],
    client_id: &str,
    user_code: &str,
    username: &str,
    password: &str,
    optional_administration: bool,
) -> Result<(), TrellisTestError> {
    let stage = TrellisTestStage::ClientLogin;
    let http = crate::runtime::https_client_builder(trust_roots)
        .map_err(|error| failure(stage, error))?
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| failure(stage, error))?;
    let cookies_of = |response: &reqwest::Response| -> Vec<String> {
        response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|header| {
                header
                    .to_str()
                    .ok()
                    .and_then(|value| value.split(';').next())
                    .map(str::to_owned)
            })
            .collect()
    };
    let location_of = |response: &reqwest::Response| {
        response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    // The verification page opens a device sign-in on a new Portal session,
    // which signs in first, as a person without a Portal login does.
    let response = http
        .get(format!("{oauth_origin}/device"))
        .send()
        .await
        .map_err(|error| failure(stage, error))?;
    let cookies = cookies_of(&response);
    let csrf = cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("__Host-trellis-portal-csrf="))
        .ok_or_else(|| failure(stage, "device page omitted CSRF cookie"))?
        .to_owned();
    let transaction = location_of(&response)
        .and_then(|location| {
            location
                .strip_prefix("/login?transaction=")
                .map(str::to_owned)
        })
        .ok_or_else(|| failure(stage, "device page did not ask to sign in"))?;
    let response = http
        .post(format!("{oauth_origin}/portal/login"))
        .header("Origin", oauth_origin)
        .header(COOKIE, cookies.join("; "))
        .form(&[
            ("csrf", csrf.clone()),
            ("username", username.into()),
            ("password", password.into()),
            ("request_id", transaction.clone()),
        ])
        .send()
        .await
        .map_err(|error| failure(stage, error))?;
    if !response.status().is_success() {
        return Err(failure(stage, "portal login denied"));
    }
    let cookie = cookies_of(&response).join("; ");
    // The code step, then the step confirming the device is this person's.
    let step = format!("/device?transaction={transaction}");
    let response = http
        .post(format!("{oauth_origin}/portal/device"))
        .header("Origin", oauth_origin)
        .header(COOKIE, &cookie)
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("user_code", user_code),
        ])
        .send()
        .await
        .map_err(|error| failure(stage, error))?;
    if location_of(&response).as_deref() != Some(step.as_str()) {
        return Err(failure(stage, "device code denied"));
    }
    let response = http
        .post(format!("{oauth_origin}/portal/device/confirm"))
        .header("Origin", oauth_origin)
        .header(COOKIE, &cookie)
        .form(&[
            ("csrf", csrf.as_str()),
            ("transaction", transaction.as_str()),
            ("decision", "confirm"),
        ])
        .send()
        .await
        .map_err(|error| failure(stage, error))?;
    if location_of(&response).as_deref() != Some(&format!("/consent?transaction={transaction}")) {
        return Err(failure(stage, "device confirmation denied"));
    }
    let response = http
        .get(format!("{oauth_origin}/portal/authorization/{transaction}"))
        .header(COOKIE, &cookie)
        .send()
        .await
        .map_err(|error| failure(stage, error))?;
    if !response.status().is_success() {
        return Err(failure(stage, "consent presentation denied"));
    }
    let view: Value = response
        .json()
        .await
        .map_err(|error| failure(stage, error))?;
    if view["clientId"].as_str() != Some(client_id)
        || view["deviceUserCode"].as_str() != Some(user_code)
    {
        return Err(failure(stage, "consent identity mismatch"));
    }
    let presentation = view["presentationId"]
        .as_str()
        .ok_or_else(|| failure(stage, "missing consent presentation"))?;
    let mut form = vec![
        ("csrf", csrf.as_str()),
        ("transaction", transaction.as_str()),
        ("presentation", presentation),
        ("decision", "approve"),
    ];
    let mut administrative = false;
    for item in view["capabilities"]
        .as_array()
        .ok_or_else(|| failure(stage, "invalid consent presentation"))?
    {
        if item["available"] != true && item["required"] == true {
            return Err(failure(stage, "required authority unavailable"));
        }
        // Administration is approved only when the request requires it,
        // unless the caller asked for optional administration too.
        if item["available"] == true
            && (item["administrative"] != true
                || item["required"] == true
                || optional_administration)
        {
            administrative |= item["administrative"] == true;
            form.push((
                "approved",
                item["capabilityId"]
                    .as_str()
                    .ok_or_else(|| failure(stage, "invalid consent item"))?,
            ));
        }
    }
    if administrative {
        form.push(("confirm_administration", "true"));
    }
    let response = http
        .post(format!("{oauth_origin}/portal/approve"))
        .header("Origin", oauth_origin)
        .header(COOKIE, &cookie)
        .form(&form)
        .send()
        .await
        .map_err(|error| failure(stage, error))?;
    let complete = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok());
    if !response.status().is_redirection() || complete != Some("/device/complete?decision=approved")
    {
        return Err(failure(
            stage,
            "consent approval did not complete device authorization",
        ));
    }
    Ok(())
}

pub(crate) struct AdminSession {
    auth: AuthClient,
    webhooks: crate::apis::trellis_webhooks_v1::Client,
    pub(crate) trust: RuntimeAuthorizationTrust,
    pub(crate) nats_servers: Vec<String>,
}

impl AdminSession {
    pub(crate) async fn connect(
        oauth_origin: &str,
        trust_roots: &[std::path::PathBuf],
        custody: &Path,
        username: &str,
        password: &str,
    ) -> Result<Self, TrellisTestError> {
        let (session, nats_servers) = login_client(
            oauth_origin,
            trust_roots,
            custody,
            "trellis.cli",
            &json!([
                {
                    "type":"trellis_capabilities",
                    "required": [
                        "trellis.auth.v1:accounts.manage",
                        "trellis.auth.v1:users.manage",
                        "trellis.auth.v1:roles.manage",
                        "trellis.auth.v1:apis.accept",
                        "trellis.auth.v1:apis.forceReplace",
                        "trellis.auth.v1:clients.manage",
                        "trellis.auth.v1:platform.manage",
                        "trellis.auth.v1:config.read",
                        "trellis.auth.v1:config.manage",
                        "trellis.auth.v1:config.manageSecrets"
                    ],
                    "optional":crate::apps::trellis_cli::Request::OPTIONAL_CAPABILITIES
                }
            ]),
            username,
            password,
        )
        .await?;
        let trust = session.trust.clone();
        let generated = trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            nats_servers.clone(),
            30_000,
            session,
        ))
        .await
        .map_err(|error| {
            failure(
                TrellisTestStage::AdministratorLogin,
                format!("connecting native administrator SDK: {error}"),
            )
        })?;
        Ok(Self {
            webhooks: crate::apis::trellis_webhooks_v1::Client::from_generated(generated.clone()),
            auth: AuthClient::from_generated(generated),
            trust,
            nats_servers,
        })
    }

    pub(crate) async fn verify(&self) -> Result<String, TrellisTestError> {
        let response = self
            .auth
            .users_resolve(&AuthEmpty {
                extra: Default::default(),
            })
            .await
            .map_err(|error| {
                failure(
                    TrellisTestStage::AdministratorLogin,
                    format!("calling Auth.Users.Resolve: {error}"),
                )
            })?;
        if !response
            .user
            .account
            .as_ref()
            .is_some_and(|account| account.principal.enabled)
        {
            return Err(failure(
                TrellisTestStage::AdministratorLogin,
                "administrator account is disabled",
            ));
        }
        Ok(response.user.principal_id.to_string())
    }

    pub(crate) async fn create_user(
        &self,
        username: &str,
        name: &str,
        password: &str,
    ) -> Result<crate::identity::TestUserIdentity, TrellisTestError> {
        let response = self
            .auth
            .users_create(&AuthUserCreateRequest {
                given_name: None,
                family_name: None,
                email: None,
                username: username.to_owned().into(),
                display_name: name.to_owned().into(),
                password: Some(password.to_owned().into()),
                identities: None,
                idempotency_key: wire(ulid::Ulid::generate().to_string())?,
                extra: Default::default(),
            })
            .await
            .map_err(|error| failure(TrellisTestStage::ClientLogin, error))?;
        Ok(crate::identity::TestUserIdentity {
            principal_id: response.user.principal_id.to_string(),
            username: username.to_owned(),
        })
    }

    /// Grants a declared test-user capability through ordinary administration.
    pub(crate) async fn prepare_client_capabilities(
        &self,
        principal_id: &str,
        capability_ids: &[&str],
    ) -> Result<(), TrellisTestError> {
        use futures_util::TryStreamExt;
        let grants: Vec<_> = self
            .auth
            .capability_grants_list_items(AuthCapabilityGrantsListRequest {
                capability_id: None,
                principal_id: Some(wire(principal_id)?),
                page: None,
                extra: Default::default(),
            })
            .try_collect()
            .await
            .map_err(|error| failure(TrellisTestStage::ClientLogin, error))?;
        for capability_id in capability_ids {
            if grants
                .iter()
                .any(|grant| grant.capability.capability_id.as_ref() == *capability_id)
            {
                continue;
            }
            self.auth
                .capability_grants_grant(&AuthCapabilityGrantRequest {
                    principal_id: wire(principal_id)?,
                    capability_id: (*capability_id).to_owned().into(),
                    expected_revision: 0.into(),
                    expires_at: None,
                    idempotency_key: wire(ulid::Ulid::generate().to_string())?,
                    extra: Default::default(),
                })
                .await
                .map_err(|error| failure(TrellisTestStage::ClientLogin, error))?;
        }
        Ok(())
    }

    /// Accept generated source APIs through server-reviewed catalog transactions.
    pub(crate) async fn accept_apis<P: ParticipantDescriptor>(
        &self,
    ) -> Result<(), TrellisTestError> {
        for api_id in P::IMPLEMENTED_API_IDS
            .iter()
            .chain(P::CONSUMED_API_IDS)
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let review = self
                .auth
                .apis_review(&AuthApiReviewRequest {
                    api_id: api_id.to_owned().into(),
                    package_evidence: wire(P::package_evidence())?,
                    extra: Default::default(),
                })
                .await
                .map_err(|error| failure(TrellisTestStage::ParticipantInstallation, error))?
                .review;
            match self
                .auth
                .apis_get(&AuthApiGetRequest {
                    api_id: api_id.to_owned().into(),
                    extra: Default::default(),
                })
                .await
            {
                Ok(current) if current.api.canonical_digest == review.canonical_digest => continue,
                Ok(_) => {
                    return Err(failure(
                        TrellisTestStage::ParticipantInstallation,
                        "API already accepted with a different definition; explicit catalog \
        update required",
                    ))
                }
                Err(CallError::Declared(error))
                    if matches!(
                        error.as_ref(),
                        ApisGetError::AuthError(error)
                            if error.payload().is_ok_and(|payload| payload.code.as_ref() == "not_found")
                    ) => {}
                Err(error) => {
                    return Err(failure(TrellisTestStage::ParticipantInstallation, error))
                }
            }
            if !review.compatible {
                return Err(failure(
                    TrellisTestStage::ParticipantInstallation,
                    "incompatible API requires explicit force replacement",
                ));
            }
            self.auth
                .apis_accept(&AuthApiAcceptRequest {
                    canonical_digest: review.canonical_digest,
                    expected_accepted_revision: review
                        .current_accepted_revision
                        .map_or(0, |value| value.0 .0)
                        .into(),
                    expected_generation: review
                        .current_generation
                        .map_or(0, |value| value.0 .0)
                        .into(),
                    review_id: review.review_id,
                    idempotency_key: wire(ulid::Ulid::generate().to_string())?,
                    mapping_edits: Vec::new(),
                    role_edits: Vec::new(),
                    extra: Default::default(),
                })
                .await
                .map_err(|error| failure(TrellisTestStage::ParticipantInstallation, error))?;
        }
        Ok(())
    }

    pub(crate) async fn create_deployment<P: ParticipantDescriptor>(
        &self,
        display_name: &str,
        acknowledge_shared_apis: bool,
    ) -> Result<String, TrellisTestError> {
        let response = self
            .auth
            .deployments_create(&AuthDeploymentCreateRequest {
                title: display_name.to_owned().into(),
                kind: AuthDeploymentKind::Service,
                participant_id: P::ID.to_owned(),
                package_evidence: wire(P::package_evidence())?,
                acknowledge_shared_apis: acknowledge_shared_apis.then_some(true),
                acknowledge_unauthenticated_webhooks: None,
                idempotency_key: wire(ulid::Ulid::generate().to_string())?,
                extra: Default::default(),
            })
            .await
            .map_err(|error| failure(TrellisTestStage::DeploymentCreateApply, error))?;
        if response.deployment.participant_id != P::ID {
            return Err(failure(
                TrellisTestStage::DeploymentCreateApply,
                "deployment participant mismatch",
            ));
        }
        Ok(response.deployment.deployment_id.to_string())
    }

    /// Removes a deployment at its current revision through
    /// `Auth.Deployments.Remove`.
    pub(crate) async fn remove_deployment(
        &self,
        deployment_id: &str,
    ) -> Result<(), TrellisTestError> {
        let current = self
            .auth
            .deployments_get(&wire(json!({ "deploymentId": deployment_id }))?)
            .await
            .map_err(|error| failure(TrellisTestStage::DeploymentCreateApply, error))?;
        self.auth
            .deployments_remove(&wire(json!({
                "deploymentId": deployment_id,
                "expectedRevision": current.deployment.revision,
                "idempotencyKey": ulid::Ulid::generate().to_string(),
            }))?)
            .await
            .map_err(|error| failure(TrellisTestStage::DeploymentCreateApply, error))?;
        Ok(())
    }

    /// Store settings for a deployment, or one of its instances, as a new
    /// revision through the same `Config.Set` administrators use.
    /// Each webhook endpoint of a deployment, by name, with the URL senders use.
    pub(crate) async fn webhook_urls(
        &self,
        deployment_id: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, TrellisTestError> {
        let listed = self
            .webhooks
            .endpoints_list(
                &serde_json::from_value(json!({ "service": deployment_id })).map_err(|error| {
                    failure(TrellisTestStage::AdministratorLogin, error.to_string())
                })?,
            )
            .await
            .map_err(|error| failure(TrellisTestStage::AdministratorLogin, error.to_string()))?;
        Ok(listed
            .endpoints
            .into_iter()
            .map(|endpoint| (endpoint.endpoint, endpoint.url))
            .collect())
    }

    /// Sets one runtime setting of a webhook endpoint, or with `None` returns
    /// it to its default, through `Auth.Webhooks.Update`.
    pub(crate) async fn set_webhook_setting(
        &self,
        deployment_id: &str,
        endpoint: &str,
        setting: &str,
        value: Option<u64>,
    ) -> Result<(), TrellisTestError> {
        self.auth
            .webhooks_update(&wire(json!({
                "deploymentId": deployment_id,
                "endpoint": endpoint,
                "change": "set",
                "setting": setting,
                "value": value.map(|value| value.to_string()),
                "idempotencyKey": ulid::Ulid::generate().to_string(),
            }))?)
            .await
            .map_err(|error| failure(TrellisTestStage::ServiceProvisioning, error))?;
        Ok(())
    }

    /// The newest request records of a webhook endpoint, as `Attempts.Query`
    /// returns them.
    pub(crate) async fn webhook_attempts(
        &self,
        deployment_id: &str,
        endpoint: &str,
    ) -> Result<Vec<serde_json::Value>, TrellisTestError> {
        let listed = self
            .webhooks
            .attempts_query(&wire(
                json!({ "service": deployment_id, "endpoint": endpoint }),
            )?)
            .await
            .map_err(|error| failure(TrellisTestStage::ServiceProvisioning, error.to_string()))?;
        listed
            .attempts
            .iter()
            .map(|attempt| {
                serde_json::to_value(attempt)
                    .map_err(|error| failure(TrellisTestStage::ServiceProvisioning, error))
            })
            .collect()
    }

    pub(crate) async fn set_config(
        &self,
        deployment_id: &str,
        instance_id: Option<&str>,
        values: serde_json::Value,
        secrets: serde_json::Value,
    ) -> Result<(), TrellisTestError> {
        let target =
            serde_json::json!({ "deploymentId": deployment_id, "instanceId": instance_id });
        let current = self
            .auth
            .config_get(&wire(serde_json::json!({ "target": target }))?)
            .await
            .map_err(|error| failure(TrellisTestStage::ServiceProvisioning, error))?;
        self.auth
            .config_set(&wire(serde_json::json!({
                "target": target,
                "values": values,
                "secrets": secrets,
                "expectedRevision": current.revision,
                "idempotencyKey": ulid::Ulid::generate().to_string(),
            }))?)
            .await
            .map_err(|error| failure(TrellisTestStage::ServiceProvisioning, error))?;
        Ok(())
    }

    pub(crate) async fn provision_service_instance(
        &self,
        deployment_id: &str,
    ) -> Result<(String, String), TrellisTestError> {
        let (seed, key) = trellis_rs::auth::generate_session_keypair();
        let response = self
            .auth
            .service_instances_provision(&AuthInstanceProvisionRequest {
                deployment_id: wire(deployment_id)?,
                identity_public_key: Some(key.clone().into()),
                title: "service".to_owned().into(),
                idempotency_key: wire(ulid::Ulid::generate().to_string())?,
                extra: Default::default(),
            })
            .await
            .map_err(|error| failure(TrellisTestStage::ServiceProvisioning, error))?;
        if response.instance.deployment_id.to_string() != deployment_id
            || response
                .instance
                .identity_public_key
                .as_ref()
                .map(|key| key.as_ref())
                != Some(key.as_str())
        {
            return Err(failure(
                TrellisTestStage::ServiceProvisioning,
                "provisioned identity mismatch",
            ));
        }
        Ok((response.instance.instance_id.to_string(), seed))
    }

    pub(crate) async fn register_native_client(
        &self,
        name: &str,
    ) -> Result<String, TrellisTestError> {
        let client_id = ulid::Ulid::generate().to_string();
        self.auth
            .o_auth_clients_put(&AuthOAuthClientPutRequest {
                client_id: client_id.clone().into(),
                title: name.to_owned().into(),
                description: "Native integration client".to_owned().into(),
                development: false,
                kind: AuthPublicClientKind::Native,
                redirect_uris: Vec::new(),
                implied_consent: Vec::new(),
                expected_revision: 0.into(),
                idempotency_key: wire(ulid::Ulid::generate().to_string())?,
                extra: Default::default(),
            })
            .await
            .map_err(|error| failure(TrellisTestStage::ClientLogin, error))?;
        Ok(client_id)
    }
}
