use super::super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OidcStartQuery {
    flow_id: String,
    portal_binding_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountFlowOidcStartQuery {
    browser_flow_id: Option<String>,
    portal_binding_digest: Option<String>,
}

pub(crate) async fn start_oidc<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(provider_id): Path<String>,
    Query(query): Query<OidcStartQuery>,
) -> Result<Response, HttpError>
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
    let flow = load_flow(&state.ephemeral, &query.flow_id).await?;
    if flow.state != AuthBrowserFlowState::ChooseProvider {
        return Err(HttpError::conflict("flow_not_pending"));
    }
    validate_portal_binding_digest(&query.portal_binding_digest)?;
    let (portal, settings) = state
        .service
        .repository()
        .get_login_portal(&flow.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    if portal.disabled || portal.removed || !portal.provider_ids.contains(&provider_id) {
        return Err(HttpError::forbidden("provider_not_allowed"));
    }
    begin_oidc(
        &state,
        provider_id,
        flow.flow_id,
        AuthOAuthKind::Browser,
        Some((&portal, &settings)),
        Some(query.portal_binding_digest),
        None,
    )
    .await
}

pub(crate) async fn start_account_flow_oidc<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path((flow_token, provider_id)): Path<(String, String)>,
    Query(query): Query<AccountFlowOidcStartQuery>,
) -> Result<Response, HttpError>
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
        return Err(HttpError::gone("account_flow_expired"));
    };
    if !matches!(
        flow.kind,
        super::super::super::AccountFlowKind::IdentityLink
            | super::super::super::AccountFlowKind::AdminAccount
    ) || flow.state != AccountFlowState::Pending
        || flow.expires_at < now_ms()?
        || flow
            .target_provider_id
            .as_ref()
            .is_some_and(|target| target != &provider_id)
    {
        return Err(HttpError::conflict("account_flow_not_eligible"));
    }
    if flow.kind == super::super::super::AccountFlowKind::AdminAccount
        && flow.target_principal_id.is_some()
    {
        return Err(HttpError::bad_request("account_flow_provider_not_allowed"));
    }
    let portal = if flow.kind == super::super::super::AccountFlowKind::AdminAccount {
        let portal = state
            .service
            .repository()
            .get_login_portal("builtin")
            .await?
            .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
        if portal.0.disabled || portal.0.removed || !portal.0.provider_ids.contains(&provider_id) {
            return Err(HttpError::forbidden("provider_not_allowed"));
        }
        Some(portal)
    } else {
        None
    };
    if query.browser_flow_id.is_some() != query.portal_binding_digest.is_some() {
        return Err(HttpError::bad_request("browser_flow_binding_incomplete"));
    }
    if let Some(portal_binding_digest) = query.portal_binding_digest.as_deref() {
        validate_portal_binding_digest(portal_binding_digest)?;
        let browser_flow = load_flow(
            &state.ephemeral,
            query.browser_flow_id.as_deref().expect("checked together"),
        )
        .await?;
        if browser_flow.state != AuthBrowserFlowState::ChooseProvider
            || browser_flow.portal_id != "builtin"
        {
            return Err(HttpError::conflict("oauth_flow_changed"));
        }
    }
    begin_oidc(
        &state,
        provider_id,
        flow_token,
        AuthOAuthKind::AccountFlow,
        portal.as_ref().map(|(portal, settings)| (portal, settings)),
        query.portal_binding_digest,
        query.browser_flow_id,
    )
    .await
}

async fn begin_oidc<R, E>(
    state: &AuthHttpState<R, E>,
    provider_id: String,
    flow_id: String,
    kind: AuthOAuthKind,
    portal_policy: Option<(&LoginPortalRecord, &LoginSettingsRecord)>,
    portal_binding_digest: Option<String>,
    browser_flow_id: Option<String>,
) -> Result<Response, HttpError>
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
    let provider = state
        .oidc_providers
        .get(&provider_id)
        .ok_or_else(|| HttpError::not_found("provider_not_found"))?;
    let client = CoreClient::from_provider_metadata(
        provider.metadata.clone(),
        provider.client_id.clone(),
        provider.client_secret.clone(),
    )
    .set_redirect_uri(provider.redirect_uri.clone());
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let mut authorization = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .set_pkce_challenge(pkce_challenge);
    for scope in &provider.scopes {
        authorization = authorization.add_scope(scope.clone());
    }
    let (authorization_url, csrf, nonce) = authorization.url();
    let now = now_ms()?;
    let mut browser_binding = [0_u8; 32];
    getrandom::fill(&mut browser_binding)
        .map_err(|_| HttpError::internal("oauth_browser_binding_generation"))?;
    let browser_binding = URL_SAFE_NO_PAD.encode(browser_binding);
    let browser_binding_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(&browser_binding));
    let (portal_id, portal_policy_digest) = match portal_policy {
        Some((portal, settings)) => (
            Some(portal.portal_id.clone()),
            Some(oidc_portal_policy_digest(portal, settings)?),
        ),
        None => (None, None),
    };
    state
        .ephemeral
        .create_oauth_state(AuthOAuthState {
            format: "trellis.auth-oauth-state.v1".to_owned(),
            state_id: csrf.secret().clone(),
            provider_id: provider_id.clone(),
            kind,
            flow_id,
            status: AuthOAuthStatus::Pending,
            pkce_verifier: pkce_verifier.secret().clone(),
            nonce: nonce.secret().clone(),
            redirect_uri: provider.redirect_uri.as_str().to_owned(),
            browser_binding_digest,
            portal_binding_digest,
            browser_flow_id,
            portal_id,
            portal_policy_digest,
            claim_owner: None,
            result_digest: None,
            authenticated_principal_id: None,
            authenticated_provider_subject: None,
            authenticated_email: None,
            authenticated_roles: Vec::new(),
            created_at: now,
            expires_at: checked_add(now, 15 * 60_000)?,
            version: 1,
        })
        .await?;
    let mut response = Redirect::temporary(authorization_url.as_str()).into_response();
    response.headers_mut().append(
        SET_COOKIE,
        oauth_cookie_header(
            &oauth_cookie_name(csrf.secret()),
            &browser_binding,
            &provider_id,
            state.public_origin.starts_with("https://"),
            15 * 60,
        )?,
    );
    Ok(response)
}

#[derive(Deserialize)]
pub(crate) struct OidcCallbackQuery {
    code: Option<String>,
    state: String,
    error: Option<String>,
}

pub(crate) async fn oidc_callback<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(provider_id): Path<String>,
    Query(query): Query<OidcCallbackQuery>,
    headers: HeaderMap,
) -> Response
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
    let cookie_name = oauth_cookie_name(&query.state);
    let state_id = query.state.clone();
    let mut response = match oidc_callback_inner(&state, &provider_id, query, &headers).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    let clear_cookie = match state.ephemeral.get_oauth_state(&state_id).await {
        Ok(Some(oauth)) => oauth_cookie_is_terminal(oauth.status),
        Ok(None) => true,
        Err(error) => {
            tracing::error!(%error, %state_id, "failed to inspect OAuth state after callback");
            false
        }
    };
    if clear_cookie {
        if let Ok(cookie) = oauth_cookie_header(
            &cookie_name,
            "",
            &provider_id,
            state.public_origin.starts_with("https://"),
            0,
        ) {
            response.headers_mut().append(SET_COOKIE, cookie);
        }
    }
    response
}

async fn oidc_callback_inner<R, E>(
    state: &AuthHttpState<R, E>,
    provider_id: &str,
    query: OidcCallbackQuery,
    headers: &HeaderMap,
) -> Result<Response, HttpError>
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
    let mut pending = state
        .ephemeral
        .get_oauth_state(&query.state)
        .await?
        .ok_or_else(|| HttpError::bad_request("oauth_state_invalid"))?;
    if pending.expires_at < now_ms()?
        && matches!(
            pending.status,
            AuthOAuthStatus::Pending | AuthOAuthStatus::Claimed | AuthOAuthStatus::ExchangeStarted
        )
    {
        if pending.kind == AuthOAuthKind::Browser {
            require_oauth_browser_binding(&query.state, &pending, headers)?;
        }
        let expected = pending.version;
        pending.status = AuthOAuthStatus::Expired;
        pending.version += 1;
        state
            .ephemeral
            .replace_oauth_state(expected, pending)
            .await?;
        return Err(HttpError::gone("oauth_expired"));
    }
    if pending.kind == AuthOAuthKind::AccountFlow
        && matches!(
            pending.status,
            AuthOAuthStatus::ExchangeStarted | AuthOAuthStatus::Completed
        )
        && pending.provider_id == provider_id
        && pending.expires_at >= now_ms()?
        && pending.authenticated_provider_subject.is_some()
    {
        require_oauth_browser_binding(&query.state, &pending, headers)?;
        return complete_first_admin_oauth(state, pending, now_ms()?).await;
    }
    if pending.kind == AuthOAuthKind::AccountFlow
        && pending.status == AuthOAuthStatus::ExchangeStarted
        && pending.provider_id == provider_id
        && pending.expires_at >= now_ms()?
    {
        require_oauth_browser_binding(&query.state, &pending, headers)?;
        mark_oauth_restart_required(&state.ephemeral, &mut pending).await?;
        return Err(HttpError::conflict("oauth_restart_required"));
    }
    if pending.kind == AuthOAuthKind::Browser
        && matches!(
            pending.status,
            AuthOAuthStatus::ExchangeStarted | AuthOAuthStatus::Completed
        )
        && pending.provider_id == provider_id
        && pending.expires_at >= now_ms()?
    {
        require_oauth_browser_binding(&query.state, &pending, headers)?;
        if pending.status == AuthOAuthStatus::ExchangeStarted
            && pending.authenticated_principal_id.is_some()
        {
            return complete_browser_oauth(state, pending, now_ms()?).await;
        }
        if pending.status == AuthOAuthStatus::ExchangeStarted {
            let mut pending = pending;
            mark_oauth_restart_required(&state.ephemeral, &mut pending).await?;
            return Err(HttpError::conflict("oauth_restart_required"));
        }
        let flow = load_flow(&state.ephemeral, &pending.flow_id).await?;
        if pending.status == AuthOAuthStatus::Completed
            && matches!(
                flow.state,
                AuthBrowserFlowState::ApprovalRequired
                    | AuthBrowserFlowState::Approved
                    | AuthBrowserFlowState::Consumed
            )
        {
            let (portal, _) = state
                .service
                .repository()
                .get_login_portal(&flow.portal_id)
                .await?
                .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
            return Ok(Redirect::temporary(&super::request::portal_url(
                &portal,
                &state.public_origin,
                &flow.flow_id,
            )?)
            .into_response());
        }
    }
    if pending.status != AuthOAuthStatus::Pending
        || pending.provider_id != provider_id
        || pending.expires_at < now_ms()?
    {
        return Err(HttpError::bad_request("oauth_state_invalid"));
    }
    require_oauth_browser_binding(&query.state, &pending, headers)?;
    let provider = state
        .oidc_providers
        .get(provider_id)
        .ok_or_else(|| HttpError::not_found("provider_not_found"))?;
    if pending.redirect_uri != provider.redirect_uri.as_str() {
        return Err(HttpError::bad_request("oauth_state_invalid"));
    }
    let federated_registration_enabled = if let Some(portal_id) = pending.portal_id.as_deref() {
        let (portal, settings) = state
            .service
            .repository()
            .get_login_portal(portal_id)
            .await?
            .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
        if portal.disabled
            || portal.removed
            || !portal.provider_ids.iter().any(|value| value == provider_id)
            || pending.portal_policy_digest.as_deref()
                != Some(oidc_portal_policy_digest(&portal, &settings)?.as_str())
        {
            return Err(HttpError::conflict("oauth_policy_changed"));
        }
        settings.federated_registration_enabled
    } else {
        false
    };
    match pending.kind {
        AuthOAuthKind::Browser => {
            let flow = load_flow(&state.ephemeral, &pending.flow_id).await?;
            if flow.state != AuthBrowserFlowState::ChooseProvider
                || pending.portal_id.as_deref() != Some(flow.portal_id.as_str())
            {
                return Err(HttpError::conflict("oauth_flow_changed"));
            }
        }
        AuthOAuthKind::AccountFlow => {
            let Some((_, flow)) =
                load_account_flow_by_token(state.service.repository(), &pending.flow_id).await?
            else {
                return Err(HttpError::gone("account_flow_expired"));
            };
            if !matches!(
                flow.kind,
                super::super::super::AccountFlowKind::IdentityLink
                    | super::super::super::AccountFlowKind::AdminAccount
            ) || flow.state != AccountFlowState::Pending
                || flow.expires_at < now_ms()?
                || flow
                    .target_provider_id
                    .as_deref()
                    .is_some_and(|target| target != provider_id)
                || (flow.kind == super::super::super::AccountFlowKind::AdminAccount
                    && pending.portal_id.as_deref() != Some("builtin"))
                || (flow.kind == super::super::super::AccountFlowKind::IdentityLink
                    && pending.portal_id.is_some())
            {
                return Err(HttpError::conflict("oauth_flow_changed"));
            }
            if let Some(browser_flow_id) = pending.browser_flow_id.as_deref() {
                if pending.portal_binding_digest.is_none() {
                    return Err(HttpError::conflict("oauth_flow_changed"));
                }
                let browser_flow = load_flow(&state.ephemeral, browser_flow_id).await?;
                if browser_flow.state != AuthBrowserFlowState::ChooseProvider
                    || browser_flow.portal_id != "builtin"
                {
                    return Err(HttpError::conflict("oauth_flow_changed"));
                }
            }
        }
    }
    let claim_owner = format!("callback_{}", URL_SAFE_NO_PAD.encode(getrandom_bytes()?));
    let mut oauth = claim_oauth_state(&state.ephemeral, &query.state, &claim_owner).await?;
    if query.error.is_some() {
        let expected = oauth.version;
        oauth.status = AuthOAuthStatus::Expired;
        oauth.version += 1;
        state.ephemeral.replace_oauth_state(expected, oauth).await?;
        return Err(HttpError::bad_request("oauth_denied"));
    }
    let code = query
        .code
        .ok_or_else(|| HttpError::bad_request("oauth_code_missing"))?;
    let expected = oauth.version;
    oauth.status = AuthOAuthStatus::ExchangeStarted;
    oauth.version += 1;
    state
        .ephemeral
        .replace_oauth_state(expected, oauth.clone())
        .await?;
    let client = CoreClient::from_provider_metadata(
        provider.metadata.clone(),
        provider.client_id.clone(),
        provider.client_secret.clone(),
    )
    .set_redirect_uri(provider.redirect_uri.clone());
    let http_client = openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| HttpError::internal("oauth_http_client"))?;
    let token = match client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|_| HttpError::bad_request("oauth_exchange_invalid"))?
        .set_pkce_verifier(PkceCodeVerifier::new(oauth.pkce_verifier.clone()))
        .request_async(&http_client)
        .await
    {
        Ok(token) => token,
        Err(_) => {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::bad_gateway("oauth_exchange_failed"));
        }
    };
    let id_token = match token.id_token() {
        Some(id_token) => id_token,
        None => {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::bad_gateway("oauth_id_token_missing"));
        }
    };
    let verifier = client.id_token_verifier();
    let claims = match id_token.claims(&verifier, &Nonce::new(oauth.nonce.clone())) {
        Ok(claims) => claims,
        Err(_) => {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::unauthorized("oauth_id_token_invalid"));
        }
    };
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            token.access_token(),
            id_token
                .signing_alg()
                .map_err(|_| HttpError::unauthorized("oauth_id_token_invalid"))?,
            id_token
                .signing_key(&verifier)
                .map_err(|_| HttpError::unauthorized("oauth_id_token_invalid"))?,
        )
        .map_err(|_| HttpError::unauthorized("oauth_id_token_invalid"))?;
        if actual != *expected {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::unauthorized("oauth_access_token_mismatch"));
        }
    }
    let provider_attributes = ProviderLoginAttributes {
        provider_id: provider_id.to_owned(),
        roles: extract_provider_roles(&id_token.to_string(), &provider.role_claims)?,
    };
    let subject = claims.subject().as_str().to_owned();
    let now = now_ms()?;
    if oauth.kind == AuthOAuthKind::AccountFlow {
        let Some((token_hash, flow)) =
            load_account_flow_by_token(state.service.repository(), &oauth.flow_id).await?
        else {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::gone("account_flow_expired"));
        };
        if !matches!(
            flow.kind,
            super::super::super::AccountFlowKind::IdentityLink
                | super::super::super::AccountFlowKind::AdminAccount
        ) || flow.state != AccountFlowState::Pending
            || flow.expires_at < now
            || flow
                .target_provider_id
                .as_ref()
                .is_some_and(|target| target != provider_id)
        {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::conflict("account_flow_not_eligible"));
        }
        if flow.kind == super::super::super::AccountFlowKind::AdminAccount {
            if state
                .service
                .repository()
                .get_provider_identity(provider_id, &subject)
                .await?
                .is_some()
            {
                mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
                return Err(HttpError::unauthorized("federated_login_denied"));
            }
            let expected = oauth.version;
            oauth.authenticated_provider_subject = Some(subject);
            oauth.authenticated_email = claims.email().map(|email| email.as_str().to_owned());
            oauth.authenticated_roles = provider_attributes.roles;
            oauth.version += 1;
            state
                .ephemeral
                .replace_oauth_state(expected, oauth.clone())
                .await?;
            return complete_first_admin_oauth(state, oauth, now).await;
        }
        let principal_id = flow
            .target_principal_id
            .clone()
            .ok_or_else(|| HttpError::internal("identity_link_target_missing"))?;
        if state
            .service
            .repository()
            .get_provider_identity(provider_id, &subject)
            .await?
            .is_some()
        {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::conflict("provider_identity_already_linked"));
        }
        let digest = digest_parts(&[provider_id, &subject, &oauth.state_id]);
        let outcome = state
            .service
            .complete_identity_link(CompleteIdentityLinkInput {
                token: oauth.flow_id.clone(),
                expected_flow_version: flow.version,
                identity: super::super::super::ProviderIdentityLink {
                    provider: provider_id.to_owned(),
                    provider_subject: subject,
                    principal_id: principal_id.clone(),
                    linked_at: now,
                    last_seen_at: now,
                },
                local_password: None,
                completed_at: now,
                idempotency: idempotency(
                    &token_hash,
                    "account.identity.link",
                    &principal_id,
                    &oauth.state_id,
                    &digest,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?;
        if matches!(outcome, IdempotentOutcome::Replayed(_)) {
            return Err(HttpError::conflict("account_flow_consumed"));
        }
        let expected = oauth.version;
        oauth.status = AuthOAuthStatus::Completed;
        oauth.result_digest = Some(digest_parts(&[&principal_id]));
        oauth.version += 1;
        state.ephemeral.replace_oauth_state(expected, oauth).await?;
        return Ok(Redirect::temporary(&format!(
            "{}/login/account/complete",
            state.public_origin.trim_end_matches('/')
        ))
        .into_response());
    }
    let principal_id = if let Some(identity) = state
        .service
        .repository()
        .get_provider_identity(provider_id, &subject)
        .await?
    {
        let principal = state
            .service
            .repository()
            .get_principal(&identity.principal_id)
            .await?
            .ok_or_else(|| HttpError::internal("provider_principal_missing"))?;
        if principal.state != super::super::super::PrincipalState::Active {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::forbidden("account_inactive"));
        }
        principal.principal_id
    } else {
        if !federated_registration_enabled {
            mark_oauth_restart_required(&state.ephemeral, &mut oauth).await?;
            return Err(HttpError::unauthorized("federated_login_denied"));
        }
        let claim_digest = digest_parts(&[provider_id, &subject, &oauth.state_id]);
        match state
            .service
            .create_federated_user(CreateFederatedUserInput {
                provider: provider_id.to_owned(),
                provider_subject: subject,
                name: None,
                email: claims.email().map(|email| email.as_str().to_owned()),
                image: None,
                created_at: now,
                idempotency: idempotency(
                    &oauth.state_id,
                    "oauth.account.bind",
                    provider_id,
                    &oauth.state_id,
                    &claim_digest,
                    now,
                )?,
                actions: Vec::new(),
            })
            .await?
        {
            IdempotentOutcome::Applied(account) => account.principal.principal_id,
            IdempotentOutcome::Replayed(value) => value
                .get("principalId")
                .and_then(Value::as_str)
                .ok_or_else(|| HttpError::internal("invalid_account_replay"))?
                .to_owned(),
        }
    };
    let expected = oauth.version;
    oauth.authenticated_principal_id = Some(principal_id);
    oauth.authenticated_roles = provider_attributes.roles;
    oauth.version += 1;
    state
        .ephemeral
        .replace_oauth_state(expected, oauth.clone())
        .await?;
    complete_browser_oauth(state, oauth, now).await
}

async fn complete_browser_oauth<R, E>(
    state: &AuthHttpState<R, E>,
    mut oauth: AuthOAuthState,
    now: i64,
) -> Result<Response, HttpError>
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
    let principal_id = oauth
        .authenticated_principal_id
        .clone()
        .ok_or_else(|| HttpError::internal("oauth_result_missing_principal"))?;
    let flow = load_flow(&state.ephemeral, &oauth.flow_id).await?;
    let completed = super::consent::complete_authenticated_flow(
        state,
        flow,
        principal_id.clone(),
        ProviderLoginAttributes {
            provider_id: oauth.provider_id.clone(),
            roles: oauth.authenticated_roles.clone(),
        },
        oauth
            .portal_binding_digest
            .clone()
            .ok_or_else(|| HttpError::internal("oauth_portal_binding_missing"))?,
        false,
        now,
    )
    .await?;
    let expected = oauth.version;
    oauth.status = AuthOAuthStatus::Completed;
    oauth.result_digest = Some(digest_parts(&[&principal_id]));
    oauth.version += 1;
    let state_id = oauth.state_id.clone();
    match state.ephemeral.replace_oauth_state(expected, oauth).await {
        Ok(()) => {}
        Err(AuthorizationStateError::StorageConflict) => {
            let current = state
                .ephemeral
                .get_oauth_state(&state_id)
                .await?
                .ok_or_else(|| HttpError::bad_request("oauth_state_invalid"))?;
            if current.status != AuthOAuthStatus::Completed
                || current.authenticated_principal_id.as_deref() != Some(&principal_id)
            {
                return Err(HttpError::conflict("oauth_completion_conflict"));
            }
        }
        Err(error) => return Err(error.into()),
    }
    let (portal, _) = state
        .service
        .repository()
        .get_login_portal(&completed.portal_id)
        .await?
        .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
    Ok(Redirect::temporary(&super::request::portal_url(
        &portal,
        &state.public_origin,
        &completed.flow_id,
    )?)
    .into_response())
}

async fn complete_first_admin_oauth<R, E>(
    state: &AuthHttpState<R, E>,
    mut oauth: AuthOAuthState,
    now: i64,
) -> Result<Response, HttpError>
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
    let subject = oauth
        .authenticated_provider_subject
        .clone()
        .ok_or_else(|| HttpError::internal("oauth_result_missing_subject"))?;
    let Some((token_hash, flow)) =
        load_account_flow_by_token(state.service.repository(), &oauth.flow_id).await?
    else {
        return Err(HttpError::gone("account_flow_expired"));
    };
    if flow.kind != super::super::super::AccountFlowKind::AdminAccount
        || !matches!(
            flow.state,
            AccountFlowState::Pending | AccountFlowState::Consumed
        )
    {
        return Err(HttpError::conflict("account_flow_not_eligible"));
    }
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
    let digest = digest_parts(&[&oauth.provider_id, &subject, &oauth.state_id]);
    let outcome = state
        .service
        .complete_first_admin_federated(FirstAdminFederatedRegistration {
            token: oauth.flow_id.clone(),
            expected_flow_version: flow.version,
            provider: oauth.provider_id.clone(),
            provider_subject: subject,
            display_name: None,
            email: oauth.authenticated_email.clone(),
            image_url: None,
            bindings,
            authority_expires_at: None,
            completed_at: now,
            idempotency: idempotency(
                &token_hash,
                "first_admin.federated.complete",
                "system:first-admin",
                &oauth.state_id,
                &digest,
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
    if oauth.authenticated_principal_id.as_deref() != Some(&principal_id) {
        let expected = oauth.version;
        oauth.authenticated_principal_id = Some(principal_id.clone());
        oauth.version += 1;
        state
            .ephemeral
            .replace_oauth_state(expected, oauth.clone())
            .await?;
    }
    let browser_completion = if let Some(browser_flow_id) = oauth.browser_flow_id.as_deref() {
        let browser_flow = load_flow(&state.ephemeral, browser_flow_id).await?;
        Some(
            super::consent::complete_authenticated_flow(
                state,
                browser_flow,
                principal_id.clone(),
                ProviderLoginAttributes {
                    provider_id: oauth.provider_id.clone(),
                    roles: oauth.authenticated_roles.clone(),
                },
                oauth
                    .portal_binding_digest
                    .clone()
                    .ok_or_else(|| HttpError::internal("oauth_portal_binding_missing"))?,
                true,
                now,
            )
            .await?,
        )
    } else {
        None
    };
    if oauth.status != AuthOAuthStatus::Completed {
        let expected = oauth.version;
        oauth.status = AuthOAuthStatus::Completed;
        oauth.result_digest = Some(digest_parts(&[&principal_id]));
        oauth.version += 1;
        state.ephemeral.replace_oauth_state(expected, oauth).await?;
    }
    if let Some(completed) = browser_completion {
        let (portal, _) = state
            .service
            .repository()
            .get_login_portal(&completed.portal_id)
            .await?
            .ok_or_else(|| HttpError::gone("portal_unavailable"))?;
        return Ok(Redirect::temporary(&super::request::portal_url(
            &portal,
            &state.public_origin,
            &completed.flow_id,
        )?)
        .into_response());
    }
    Ok(Redirect::temporary(&format!(
        "{}/login/account/complete",
        state.public_origin.trim_end_matches('/')
    ))
    .into_response())
}

fn extract_provider_roles(token: &str, pointers: &[String]) -> Result<Vec<String>, HttpError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| HttpError::unauthorized("oauth_id_token_invalid"))?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| HttpError::unauthorized("oauth_id_token_invalid"))?;
    let claims: Value = serde_json::from_slice(&payload)
        .map_err(|_| HttpError::unauthorized("oauth_id_token_invalid"))?;
    let mut roles = BTreeSet::new();
    for pointer in pointers {
        let Some(value) = claims.pointer(pointer) else {
            continue;
        };
        match value {
            Value::String(role) if !role.is_empty() => {
                roles.insert(role.clone());
            }
            Value::Array(values) => {
                for value in values {
                    let Value::String(role) = value else {
                        return Err(HttpError::unauthorized("oauth_role_claim_invalid"));
                    };
                    if role.is_empty() {
                        return Err(HttpError::unauthorized("oauth_role_claim_invalid"));
                    }
                    roles.insert(role.clone());
                }
            }
            _ => return Err(HttpError::unauthorized("oauth_role_claim_invalid")),
        }
    }
    Ok(roles.into_iter().collect())
}

async fn mark_oauth_restart_required(
    repository: &impl AuthEphemeralRepository,
    oauth: &mut AuthOAuthState,
) -> Result<(), HttpError> {
    let expected = oauth.version;
    oauth.status = AuthOAuthStatus::RestartRequired;
    oauth.version += 1;
    repository
        .replace_oauth_state(expected, oauth.clone())
        .await
        .map_err(Into::into)
}

fn oauth_cookie_is_terminal(status: AuthOAuthStatus) -> bool {
    matches!(
        status,
        AuthOAuthStatus::Completed | AuthOAuthStatus::RestartRequired | AuthOAuthStatus::Expired
    )
}

#[cfg(test)]
mod role_tests {
    use super::*;

    #[test]
    fn browser_cookie_is_cleared_only_for_terminal_oauth_states() {
        for status in [
            AuthOAuthStatus::Pending,
            AuthOAuthStatus::Claimed,
            AuthOAuthStatus::ExchangeStarted,
        ] {
            assert!(!oauth_cookie_is_terminal(status));
        }
        for status in [
            AuthOAuthStatus::Completed,
            AuthOAuthStatus::RestartRequired,
            AuthOAuthStatus::Expired,
        ] {
            assert!(oauth_cookie_is_terminal(status));
        }
    }

    fn token(claims: Value) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    #[test]
    fn extracts_string_array_and_nested_roles_exactly() {
        let roles = extract_provider_roles(
            &token(json!({
                "role": "Admin",
                "groups": ["Reader", "Admin"],
                "realm": { "roles": ["writer"] }
            })),
            &[
                "/role".to_owned(),
                "/groups".to_owned(),
                "/realm/roles".to_owned(),
                "/missing".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(roles, ["Admin", "Reader", "writer"]);
    }

    #[test]
    fn rejects_present_invalid_or_empty_role_claims() {
        assert!(extract_provider_roles(
            &token(json!({ "roles": ["ok", 1] })),
            &["/roles".to_owned()]
        )
        .is_err());
        assert!(
            extract_provider_roles(&token(json!({ "role": "" })), &["/role".to_owned()]).is_err()
        );
    }
}
