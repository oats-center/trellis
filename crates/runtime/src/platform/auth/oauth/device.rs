//! RFC 8628 stays in the OAuth engine. This sealed host extension binds the
//! requested Trellis authority to the persistent DPoP key before code issuance.

use super::super::sqlite::blocking;
use super::super::{GrantApproval, Mutation};
use super::{OAuthEngine, RequestedAuthority, TokenBinding};
use axum::{
    extract::{Form, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use oauth_as::events::{Attempt, RateLimitDecision, RateLimiter};
use oauth_as::{
    client::ClientId,
    device::DeviceAuthorizationResponse,
    error::{ErrorCode, ErrorResponse},
    store::Storage,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Deserialize, Serialize)]
pub(super) struct DeviceSelection {
    pub(super) client_id: String,
    pub(super) authorization_details: String,
    pub(super) persistent_dpop_jkt: String,
    pub(super) public_jwk: serde_json::Value,
}

pub(super) async fn start(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if headers.contains_key(header::AUTHORIZATION)
        || headers.get_all("dpop").iter().count() != 1
        || headers.contains_key(header::ORIGIN)
    {
        return super::http::error(ErrorCode::InvalidRequest);
    }
    let Some(proof) = headers.get("dpop").and_then(|header| header.to_str().ok()) else {
        return super::http::error(ErrorCode::InvalidDpopProof);
    };
    let fields = match super::http::fields(pairs) {
        Ok(fields) => fields,
        Err(response) => return super::http::error(response.error),
    };
    let (Some(client), Some(details), Some(runtime), Some(key)) = (
        fields.get("client_id"),
        fields.get("authorization_details"),
        fields.get("trellis_runtime_id"),
        fields.get("trellis_session_nkey"),
    ) else {
        return super::http::error(ErrorCode::InvalidRequest);
    };
    match engine
        .device_authorization(&ClientId::new(client), details, proof, runtime, key)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(response) => super::http::error(response.error),
    }
}

pub(super) async fn page(State(engine): State<Arc<OAuthEngine>>, headers: HeaderMap) -> Response {
    let now = crate::clock::unix_now();
    let authority = engine.server.store().authority.clone();
    let cookies = super::http::portal_cookie(&headers, "__Host-trellis-portal").zip(
        super::http::portal_cookie(&headers, "__Host-trellis-portal-csrf"),
    );
    let guest = blocking(move || {
        if cookies.is_some_and(|(cookie, csrf)| {
            authority
                .portal_session_identity(&cookie, &csrf, now)
                .is_ok()
        }) {
            return Ok(None);
        }
        authority.create_portal_session(now).map(Some)
    })
    .await;
    match guest {
        Ok(None) => super::assets::page(&engine, None),
        Ok(Some(guest)) => super::http::cookie_response(
            &guest.cookie,
            Some(&guest.csrf),
            600,
            super::assets::page(&engine, None),
        ),
        Err(_) => super::http::error(ErrorCode::ServerError),
    }
}

/// Explicit entry/confirmation is separate from approval: deep links never make
/// an authority decision and the normal consent page shows the actual client.
pub(super) async fn begin(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if !super::http::portal_origin(&engine, &headers) {
        return super::http::error(ErrorCode::InvalidRequest);
    }
    let form = match super::http::fields(pairs) {
        Ok(form) => form,
        Err(response) => return super::http::error(response.error),
    };
    let (Some(code), Some(csrf), Some(cookie)) = (
        form.get("user_code"),
        form.get("csrf"),
        super::http::portal_cookie(&headers, "__Host-trellis-portal"),
    ) else {
        return super::http::error(ErrorCode::InvalidRequest);
    };
    if code.len() > 32 {
        return super::http::error(ErrorCode::InvalidRequest);
    }
    let now = crate::clock::unix_now();
    let authority = engine.server.store().authority.clone();
    let (identity_cookie, identity_csrf) = (cookie.clone(), csrf.clone());
    let session = match blocking(move || {
        authority.portal_session_identity(&identity_cookie, &identity_csrf, now)
    })
    .await
    {
        Ok(session) => session,
        Err(_) => return super::http::error(ErrorCode::AccessDenied),
    };
    if !matches!(
        engine.device_entries.check(Attempt::DeviceUserCodeEntry),
        RateLimitDecision::Allow
    ) {
        return super::http::error(ErrorCode::SlowDown);
    }
    let grant = match engine
        .server
        .store()
        .find_device_grant_by_user_code(&oauth_as::device::normalize_user_code(code))
        .await
    {
        Ok(Some(grant)) => grant,
        _ => return super::http::error(ErrorCode::InvalidRequest),
    };
    if grant.expires_at <= SystemTime::now()
        || grant.state != oauth_as::device::DeviceGrantState::Pending
    {
        return super::http::error(ErrorCode::InvalidRequest);
    }
    let store = engine.server.store().clone();
    let csrf = csrf.clone();
    let started = blocking(move || {
        let selection = store
            .device_selection(&grant.device_code)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        let transaction = super::super::sqlite::id();
        let flow = super::portal::PortalAuthorization {
            presentation: None,
            session_id: session,
            request: super::portal::ConsentRequest::Device {
                device_code: grant.device_code,
            },
            public_jwk: selection.public_jwk,
            expires_at: grant
                .expires_at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_secs() as i64)
                .unwrap_or(now),
        };
        store
            .store_authorization(&transaction, &flow, grant.client_id.as_str())
            .map_err(|_| ErrorCode::ServerError)?;
        let page = if store.authority.portal_login(&cookie, &csrf, now).is_ok() {
            "consent"
        } else {
            "login"
        };
        Ok((page, transaction))
    })
    .await;
    let (page, transaction) = match started {
        Ok(started) => started,
        Err(code) => return super::http::error(code),
    };
    Redirect::to(&format!("/{page}?transaction={transaction}")).into_response()
}

impl OAuthEngine {
    /// Issue a real engine-owned device/user-code pair only after authenticated
    /// DPoP extension validation and a durable single-use proof claim.
    pub(crate) async fn device_authorization(
        &self,
        client_id: &ClientId,
        details: &str,
        proof: &str,
        runtime_id: &str,
        ephemeral_nkey: &str,
    ) -> Result<DeviceAuthorizationResponse, ErrorResponse> {
        let requested = RequestedAuthority::parse(details)?;
        self.prepare_client(client_id).await?;
        let authority = self.server.store().authority.clone();
        let (client, selection) = (
            client_id.as_str().to_owned(),
            requested.capabilities.clone(),
        );
        blocking(move || {
            authority.transaction(|connection| {
                super::enrollment::validate_selection(
                    connection,
                    &client,
                    &selection.required,
                    &selection.optional,
                )
            })
        })
        .await
        .map_err(|_| ErrorResponse::new(ErrorCode::InvalidRequest))?;
        let uri = format!(
            "{}/device_authorization",
            self.server.store().authority.settings.oauth_origin
        );
        let binding = TokenBinding::verify(
            &self.verifiers,
            proof,
            &uri,
            runtime_id,
            ephemeral_nkey,
            SystemTime::now(),
        )
        .map_err(|_| ErrorResponse::new(ErrorCode::InvalidDpopProof))?;
        // This host-owned endpoint has its own URI-bound replay namespace; the
        // engine owns its token-endpoint proof claims independently.
        let replay = format!(
            "trellis.device.dpop:{}:{}",
            binding.persistent_dpop_jkt, binding.jti
        );
        if !self
            .server
            .store()
            .claim_replay_id(&replay, binding.replay_until)
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?
        {
            return Err(ErrorResponse::new(ErrorCode::InvalidDpopProof));
        }
        let response = self
            .server
            .device_authorization(client_id, None, None)
            .await?;
        let grant = self
            .server
            .store()
            .get_device_grant(&response.device_code)
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?
            .ok_or_else(|| ErrorResponse::new(ErrorCode::ServerError))?;
        let store = self.server.store().clone();
        let selection = DeviceSelection {
            client_id: client_id.as_str().into(),
            authorization_details: details.into(),
            persistent_dpop_jkt: binding.persistent_dpop_jkt,
            public_jwk: binding.public_jwk,
        };
        blocking(move || store.store_device_selection(&grant, &selection))
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?;
        Ok(response)
    }

    /// Called only after portal login and CSRF-protected capability selection.
    pub(crate) async fn approve_device(
        &self,
        user_code: &str,
        identity: &Mutation,
        approval: &GrantApproval,
    ) -> Result<(), super::portal::ApprovalError> {
        let device = self
            .server
            .store()
            .find_device_grant_by_user_code(&oauth_as::device::normalize_user_code(user_code))
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::ServerError))?
            .ok_or_else(|| ErrorResponse::new(ErrorCode::InvalidGrant))?;
        let (store, device_code) = (self.server.store().clone(), device.device_code.clone());
        let selection = blocking(move || store.device_selection(&device_code))
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::InvalidGrant))?;
        let requested = RequestedAuthority::parse(&selection.authorization_details)?;
        if device.client_id.as_str() != approval.client_id
            || selection.client_id != approval.client_id
            || selection.persistent_dpop_jkt != approval.durable_dpop_jkt
            || approval.binding_kind != "native"
            || approval.binding_value != selection.persistent_dpop_jkt
            || approval.public_jwk != selection.public_jwk
            || requested.capabilities.required != approval.selection.required
            || requested.capabilities.optional != approval.selection.optional
            || requested.required_privileges != approval.required_privileges
            || !requested
                .required_privileges
                .iter()
                .all(|privilege| approval.privileges.contains(privilege))
            || !approval.privileges.iter().all(|privilege| {
                requested.required_privileges.contains(privilege)
                    || requested.optional_privileges.contains(privilege)
            })
        {
            return Err(ErrorResponse::new(ErrorCode::InvalidGrant).into());
        }
        let now = crate::clock::unix_now();
        let authority = self.server.store().authority.clone();
        let (identity, approval) = (identity.clone(), approval.clone());
        let grant = blocking(move || authority.approve_grant(&identity, &approval, now))
            .await
            .map_err(super::portal::ApprovalError::from)?;
        self.server
            .approve_device(user_code, grant)
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::InvalidGrant).into())
    }
}
