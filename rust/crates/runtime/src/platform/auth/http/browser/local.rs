use super::super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserFlowResponse {
    pub(crate) flow_id: String,
    pub(crate) state: AuthBrowserFlowState,
    pub(crate) expires_at: i64,
    pub(crate) providers: Vec<String>,
    pub(crate) registration_enabled: bool,
    pub(crate) federated_registration_enabled: bool,
    pub(crate) consent_view: Value,
    pub(crate) redirect_target: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PortalFlowResponse {
    #[serde(flatten)]
    flow: BrowserFlowResponse,
    decision_digest: String,
    user: BrowserFlowUser,
}

pub(super) async fn portal_flow_response<R, E>(
    state: &AuthHttpState<R, E>,
    flow: AuthBrowserFlow,
) -> Result<PortalFlowResponse, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let principal_id = flow
        .principal_id
        .as_deref()
        .ok_or_else(|| HttpError::conflict("flow_not_authenticated"))?;
    let profile = state
        .service
        .repository()
        .get_user_profile(principal_id)
        .await?
        .ok_or_else(|| HttpError::internal("flow_principal_missing"))?;
    Ok(PortalFlowResponse {
        decision_digest: flow.consent.decision_digest.clone(),
        user: BrowserFlowUser {
            origin: "trellis",
            id: profile.principal_id,
            name: profile.display_name,
            email: profile.email,
            image: profile.image_url,
        },
        flow: flow_response(flow),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserFlowUser {
    origin: &'static str,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
}

pub(crate) async fn get_flow<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_id): Path<String>,
) -> Result<Json<BrowserFlowResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let flow = load_flow(&state.ephemeral, &flow_id).await?;
    let (portal, settings) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    let providers = portal
        .provider_ids
        .iter()
        .filter(|provider| {
            (provider.as_str() == "local" && settings.local_login_enabled)
                || state.oidc_providers.contains_key(provider.as_str())
        })
        .cloned()
        .collect();
    let mut response = flow_response(flow);
    response.flow_id = flow_id;
    response.providers = providers;
    response.registration_enabled =
        portal.local_registration_enabled && settings.local_login_enabled;
    response.federated_registration_enabled = settings.federated_registration_enabled;
    Ok(Json(response))
}

pub(crate) async fn get_portal_flow<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<PortalFlowResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let flow = load_flow(&state.ephemeral, &flow_id).await?;
    let (portal, _) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    require_selected_portal_origin(&headers, &portal, &state.public_origin)?;
    require_portal_binding(&flow, &headers)?;
    Ok(Json(portal_flow_response(&state, flow).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalLoginRequest {
    flow_id: String,
    username: String,
    password: String,
    portal_binding_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountFlowResponse {
    status: &'static str,
    flow_id: Option<String>,
    kind: Option<super::super::super::AccountFlowKind>,
    mode: Option<&'static str>,
    username: Option<String>,
    allowed_providers: Option<Vec<String>>,
    expires_at: Option<i64>,
    password_policy: Option<AccountFlowPasswordPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<AccountFlowTarget>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountFlowTarget {
    user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    active: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountFlowPasswordPolicy {
    min_length: usize,
}

pub(crate) async fn get_account_flow<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_token): Path<String>,
) -> Result<Json<AccountFlowResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let Some((_, flow)) =
        load_account_flow_by_token(state.service.repository(), &flow_token).await?
    else {
        return Ok(Json(AccountFlowResponse {
            status: "expired",
            flow_id: None,
            kind: None,
            mode: None,
            username: None,
            allowed_providers: None,
            expires_at: None,
            password_policy: None,
            target: None,
        }));
    };
    let status = if flow.state == AccountFlowState::Consumed {
        "consumed"
    } else if flow.state != AccountFlowState::Pending || flow.expires_at < now_ms()? {
        "expired"
    } else {
        "pending"
    };
    let username = if flow.kind == super::super::super::AccountFlowKind::AdminAccount {
        match flow.target_principal_id.as_deref() {
            Some(principal_id) => state
                .service
                .repository()
                .get_local_credential(principal_id)
                .await?
                .map(|credential| credential.normalized_username),
            None => None,
        }
    } else {
        None
    };
    let target = if let Some(principal_id) = flow.target_principal_id.as_deref() {
        let profile = state
            .service
            .repository()
            .get_user_profile(principal_id)
            .await?;
        let principal = state
            .service
            .repository()
            .get_principal(principal_id)
            .await?;
        profile.map(|p| AccountFlowTarget {
            user_id: p.principal_id,
            name: p.display_name,
            email: p.email,
            active: principal
                .is_some_and(|pr| pr.state == super::super::super::domain::PrincipalState::Active),
        })
    } else {
        None
    };
    Ok(Json(AccountFlowResponse {
        status,
        flow_id: Some(flow_token),
        kind: Some(flow.kind),
        mode: (flow.kind == super::super::super::AccountFlowKind::AdminAccount).then_some(
            if flow.target_principal_id.is_some() {
                "edit"
            } else {
                "create"
            },
        ),
        username,
        allowed_providers: if flow.kind == super::super::super::AccountFlowKind::AdminAccount {
            Some(admin_account_providers(
                flow.target_principal_id.is_some(),
                state.oidc_providers.keys(),
            ))
        } else {
            flow.payload
                .get("allowedProviders")
                .and_then(serde_json::Value::as_array)
                .map(|providers| {
                    providers
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
        },
        expires_at: Some(flow.expires_at),
        password_policy: matches!(
            flow.kind,
            super::super::super::AccountFlowKind::AdminAccount
                | super::super::super::AccountFlowKind::PasswordReset
        )
        .then(|| AccountFlowPasswordPolicy {
            min_length: state.service.password_min_length(),
        }),
        target,
    }))
}

fn admin_account_providers<'a>(
    edit: bool,
    oidc_provider_ids: impl Iterator<Item = &'a String>,
) -> Vec<String> {
    let mut providers = vec!["local".to_owned()];
    if !edit {
        providers.extend(oidc_provider_ids.cloned());
    }
    providers
}

#[cfg(test)]
mod tests {
    #[test]
    fn admin_account_create_offers_local_and_oidc_while_edit_is_local_only() {
        let oidc = ["github".to_owned(), "google".to_owned()];
        assert_eq!(
            super::admin_account_providers(false, oidc.iter()),
            ["local", "github", "google"],
        );
        assert_eq!(super::admin_account_providers(true, oidc.iter()), ["local"],);
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminAccountRequest {
    username: Option<String>,
    password: String,
    name: Option<String>,
    email: Option<String>,
    browser_flow_id: Option<String>,
    portal_binding_digest: Option<String>,
}

pub(crate) async fn complete_admin_account<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_token): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AdminAccountRequest>,
) -> Result<Json<Value>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    if std::iter::once(&state.public_origin)
        .chain(&state.allowed_redirect_origins)
        .all(|origin| require_portal_origin(&headers, origin).is_err())
    {
        return Err(HttpError::forbidden("origin_mismatch"));
    }
    let (token_hash, flow) = load_account_flow_by_token(state.service.repository(), &flow_token)
        .await?
        .ok_or_else(|| HttpError::gone("account_flow_expired"))?;
    let is_first_admin_continuation_retry = flow.kind
        == super::super::super::AccountFlowKind::AdminAccount
        && flow.state == AccountFlowState::Consumed
        && request.browser_flow_id.is_some();
    if (!is_first_admin_continuation_retry && flow.state != AccountFlowState::Pending)
        || (flow.state == AccountFlowState::Pending && flow.expires_at < now_ms()?)
    {
        return Err(HttpError::conflict("account_flow_consumed"));
    }
    let now = now_ms()?;
    let admin_account_edit = flow.kind == super::super::super::AccountFlowKind::AdminAccount
        && flow.target_principal_id.is_some();
    let request_digest = trellis_protocol::digest_json(
        &serde_json::to_value(&request)
            .map_err(|_| HttpError::bad_request("invalid_account_flow"))?,
    )
    .map_err(|_| HttpError::bad_request("invalid_account_flow"))?;
    if flow.kind == super::super::super::AccountFlowKind::PasswordReset {
        let outcome = state
            .service
            .complete_password_reset(CompletePasswordResetInput {
                token: flow_token,
                expected_flow_version: flow.version,
                username: request.username,
                bindings: Vec::new(),
                profile: None,
                password: request.password,
                consumed_at: now,
                idempotency: idempotency(
                    &token_hash,
                    "account.password.reset",
                    flow.target_principal_id
                        .as_deref()
                        .unwrap_or("account-flow"),
                    &token_hash,
                    &request_digest,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        return match outcome {
            IdempotentOutcome::Applied(flow) => Ok(Json(json!({
                "status": "updated",
                "userId": flow.target_principal_id,
            }))),
            IdempotentOutcome::Replayed(_) => Err(HttpError::conflict("account_flow_consumed")),
        };
    }
    if flow.kind == super::super::super::AccountFlowKind::IdentityLink {
        let username = super::super::super::account::normalize_username(
            request
                .username
                .as_deref()
                .ok_or_else(|| HttpError::bad_request("username_required"))?,
        )?;
        let principal_id = flow
            .target_principal_id
            .clone()
            .ok_or_else(|| HttpError::internal("identity_link_target_missing"))?;
        if state
            .service
            .repository()
            .get_provider_identity("local", &username)
            .await?
            .is_some()
        {
            return Err(HttpError::conflict("local_identity_exists"));
        }
        let outcome = state
            .service
            .complete_identity_link(CompleteIdentityLinkInput {
                token: flow_token,
                expected_flow_version: flow.version,
                identity: super::super::super::ProviderIdentityLink {
                    provider: "local".to_owned(),
                    provider_subject: username,
                    principal_id: principal_id.clone(),
                    linked_at: now,
                    last_seen_at: now,
                },
                local_password: Some(request.password),
                completed_at: now,
                idempotency: idempotency(
                    &token_hash,
                    "account.identity.link",
                    &principal_id,
                    &token_hash,
                    &request_digest,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        return match outcome {
            IdempotentOutcome::Applied(_) => Ok(Json(json!({
                "status": "created",
                "userId": principal_id,
            }))),
            IdempotentOutcome::Replayed(_) => Err(HttpError::conflict("account_flow_consumed")),
        };
    }
    if flow.kind != super::super::super::AccountFlowKind::AdminAccount {
        return Err(HttpError::bad_request("account_flow_provider_required"));
    }
    let username = request
        .username
        .clone()
        .ok_or_else(|| HttpError::bad_request("username_required"))?;
    let targets = flow.payload["bindings"]
        .as_array()
        .ok_or_else(|| HttpError::internal("first_admin_target_missing"))?;
    let mut bindings = Vec::with_capacity(targets.len());
    for target in targets {
        let participant_id = target["participantId"]
            .as_str()
            .ok_or_else(|| HttpError::internal("first_admin_target_missing"))?;
        let installed_revision = target["installedRevision"]
            .as_u64()
            .ok_or_else(|| HttpError::internal("first_admin_target_missing"))?;
        let (_, installed) = state
            .service
            .repository()
            .get_installed_participant_record(participant_id.to_owned(), Some(installed_revision))
            .await?
            .ok_or_else(|| HttpError::internal("first_admin_target_missing"))?;
        bindings.push(FirstAdminBinding {
            participant_id: participant_id.to_owned(),
            installed_revision,
            grant_set: super::complete_participant_grants(&installed)?,
            platform_privileges: (participant_id
                != crate::platform::auth::builtins::PORTAL_PARTICIPANT_ID)
                .then_some(trellis_protocol::PlatformPrivilege::Admin)
                .into_iter()
                .collect(),
        });
    }
    let browser_flow_id = request.browser_flow_id.clone();
    let browser_flow = if let Some(browser_flow_id) = browser_flow_id.as_deref() {
        let portal_binding_digest = request
            .portal_binding_digest
            .as_deref()
            .ok_or_else(|| HttpError::bad_request("portal_binding_digest_required"))?;
        validate_portal_binding_digest(portal_binding_digest)?;
        let browser_flow = load_flow(&state.ephemeral, browser_flow_id).await?;
        let (portal, _) = state
            .service
            .repository()
            .get_login_portal(&browser_flow.portal_id)
            .await?
            .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
        require_selected_portal_origin(&headers, &portal, &state.public_origin)?;
        Some(browser_flow)
    } else {
        None
    };
    let outcome = state
        .service
        .complete_first_admin(FirstAdminRegistration {
            token: flow_token,
            expected_flow_version: flow.version,
            username: username.clone(),
            password: request.password,
            display_name: request.name.filter(|value| !value.trim().is_empty()),
            email: request.email,
            image_url: None,
            bindings,
            authority_expires_at: None,
            completed_at: now,
            idempotency: idempotency(
                &token_hash,
                "first_admin.complete",
                "system:first-admin",
                &token_hash,
                &request_digest,
                now,
            )?,
            actions: Vec::new(),
        })
        .await?;
    let principal_id = match outcome {
        IdempotentOutcome::Applied(account) => account.principal.principal_id,
        IdempotentOutcome::Replayed(value) => value
            .get("principalId")
            .and_then(Value::as_str)
            .ok_or_else(|| HttpError::internal("first_admin_replay_invalid"))?
            .to_owned(),
    };
    if let Some(browser_flow) = browser_flow {
        let browser_flow_id = browser_flow.flow_id.clone();
        super::consent::complete_authenticated_flow(
            &state,
            browser_flow,
            principal_id.clone(),
            ProviderLoginAttributes {
                provider_id: "local".to_owned(),
                roles: Vec::new(),
            },
            request
                .portal_binding_digest
                .ok_or_else(|| HttpError::bad_request("portal_binding_digest_required"))?,
            true,
            now,
        )
        .await?;
        return Ok(Json(json!({
            "status": if admin_account_edit { "updated" } else { "created" },
            "userId": principal_id,
            "browserFlowId": browser_flow_id,
        })));
    }
    Ok(Json(json!({
        "status": if admin_account_edit { "updated" } else { "created" },
        "userId": principal_id,
    })))
}

pub(crate) async fn local_login<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    headers: HeaderMap,
    Json(request): Json<LocalLoginRequest>,
) -> Result<Json<BrowserFlowResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let flow = load_flow(&state.ephemeral, &request.flow_id).await?;
    let (portal, settings) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    require_selected_portal_origin(&headers, &portal, &state.public_origin)?;
    if portal.disabled
        || portal.removed
        || !settings.local_login_enabled
        || !portal
            .provider_ids
            .iter()
            .any(|provider| provider == "local")
    {
        return Err(HttpError::forbidden("local_login_disabled"));
    }
    if !matches!(
        flow.state,
        AuthBrowserFlowState::ChooseProvider | AuthBrowserFlowState::Authenticated
    ) {
        return Err(HttpError::conflict("flow_not_pending"));
    }
    let now = now_ms()?;
    let principal = match state
        .service
        .authenticate_local(&request.username, &request.password, now)
        .await?
    {
        LocalAuthentication::Authenticated { principal, .. } => principal,
        LocalAuthentication::Denied => return Err(HttpError::unauthorized("invalid_credentials")),
    };
    validate_portal_binding_digest(&request.portal_binding_digest)?;
    let completed = super::consent::complete_authenticated_flow(
        &state,
        flow,
        principal.principal_id,
        ProviderLoginAttributes {
            provider_id: "local".to_owned(),
            roles: Vec::new(),
        },
        request.portal_binding_digest,
        false,
        now,
    )
    .await?;
    let mut response = flow_response(completed);
    response.providers = vec!["local".to_owned()];
    Ok(Json(response))
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalRegistrationRequest {
    username: String,
    password: String,
    name: Option<String>,
    email: Option<String>,
    portal_binding_digest: String,
}

pub(crate) async fn register_local<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(flow_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<LocalRegistrationRequest>,
) -> Result<Json<BrowserFlowResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let flow = load_flow(&state.ephemeral, &flow_id).await?;
    if !matches!(
        flow.state,
        AuthBrowserFlowState::ChooseProvider | AuthBrowserFlowState::Authenticated
    ) {
        return Err(HttpError::conflict("flow_not_pending"));
    }
    let (portal, settings) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    require_selected_portal_origin(&headers, &portal, &state.public_origin)?;
    if portal.disabled
        || portal.removed
        || !portal.local_registration_enabled
        || !settings.local_login_enabled
        || !portal
            .provider_ids
            .iter()
            .any(|provider| provider == "local")
    {
        return Err(HttpError::forbidden("local_registration_disabled"));
    }
    let now = now_ms()?;
    validate_portal_binding_digest(&request.portal_binding_digest)?;
    let request_digest = trellis_protocol::digest_json(
        &serde_json::to_value(&request)
            .map_err(|_| HttpError::bad_request("invalid_registration"))?,
    )
    .map_err(|_| HttpError::bad_request("invalid_registration"))?;
    let account = state
        .service
        .create_local_user(CreateLocalUserInput {
            username: request.username,
            password: request.password,
            name: request.name,
            email: request.email,
            created_at: now,
            idempotency: idempotency(
                &flow_id,
                "browser.local.register",
                &flow.session_public_key,
                &flow_id,
                &request_digest,
                now,
            )?,
            actions: Vec::new(),
        })
        .await?;
    let principal_id = match account {
        IdempotentOutcome::Applied(account) => account.principal.principal_id,
        IdempotentOutcome::Replayed(value) => value
            .get("principalId")
            .and_then(Value::as_str)
            .ok_or_else(|| HttpError::internal("invalid_registration_replay"))?
            .to_owned(),
    };
    let completed = super::consent::complete_authenticated_flow(
        &state,
        flow,
        principal_id,
        ProviderLoginAttributes {
            provider_id: "local".to_owned(),
            roles: Vec::new(),
        },
        request.portal_binding_digest,
        false,
        now,
    )
    .await?;
    Ok(Json(flow_response(completed)))
}
