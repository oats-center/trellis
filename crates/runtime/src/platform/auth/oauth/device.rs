//! RFC 8628 stays in the OAuth engine. This sealed host extension binds the
//! requested Trellis authority to the persistent DPoP key before code issuance.

use super::super::sqlite::blocking;
use super::super::{GrantApproval, Mutation};
use super::{OAuthEngine, RequestedAuthority, TokenBinding};
use axum::{
    extract::{Form, Query, State},
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
    /// The network address the device asked from, shown beside its code so
    /// the person can tell their own device's request from someone else's.
    pub(super) requested_from: Option<String>,
    /// The name the device reported for itself (`trellis_device_name`),
    /// shown on the confirmation step as the device's own claim. Nothing
    /// trusts it.
    pub(super) device_name: Option<String>,
}

/// The longest device name kept, in characters.
const DEVICE_NAME_MAX_CHARS: usize = 64;

/// A device's reported name as the confirmation step shows it: control
/// characters and bidirectional formatting marks (which could reorder what
/// the page shows) become spaces, runs of whitespace collapse, and it is cut
/// to [`DEVICE_NAME_MAX_CHARS`]. A name with nothing left is no name.
fn device_name(reported: &str) -> Option<String> {
    let cleaned = reported
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let name = cleaned
        .chars()
        .take(DEVICE_NAME_MAX_CHARS)
        .collect::<String>()
        .trim_end()
        .to_owned();
    (!name.is_empty()).then_some(name)
}

pub(super) async fn start(
    State(engine): State<Arc<OAuthEngine>>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
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
        .device_authorization(
            &ClientId::new(client),
            details,
            proof,
            runtime,
            key,
            peer.map(|axum::Extension(peer)| peer.0.ip()),
            fields
                .get("trellis_device_name")
                .and_then(|name| device_name(name)),
        )
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(response) => super::http::described_error(&response),
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
        Ok(None) => super::assets::page(&engine, None, None),
        Ok(Some(guest)) => super::http::cookie_response(
            &guest.cookie,
            Some(&guest.csrf),
            600,
            super::assets::page(&engine, None, None),
        ),
        Err(cause) => super::http::error(super::http::failed(
            "could not open a Portal session for the device page",
            cause,
        )),
    }
}

/// `GET /device`, the verification URL a device shows. Without `transaction`
/// it starts a new device sign-in for this browser: someone signed in to the
/// Portal is asked whether to continue as themselves, anyone else signs in
/// first. With `transaction` it shows that sign-in's next step: the code
/// entry, then the device's details to confirm. A code is never taken from
/// the link, so a link someone else sent can't carry their device's code.
pub(super) async fn entry(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let form = match super::http::fields(pairs) {
        Ok(form) => form,
        Err(response) => return super::http::error(response.error),
    };
    let cookies = super::http::portal_cookie(&headers, "__Host-trellis-portal").zip(
        super::http::portal_cookie(&headers, "__Host-trellis-portal-csrf"),
    );
    let now = crate::clock::unix_now();
    match form.get("transaction") {
        None => start_sign_in(&engine, cookies, now).await,
        Some(transaction) if ulid::Ulid::from_string(transaction).is_ok() => {
            let account = form.get("account").is_some_and(|ask| ask == "confirm");
            step(&engine, cookies, transaction, account, now).await
        }
        Some(_) => super::http::error(ErrorCode::InvalidRequest),
    }
}

/// Opens a device sign-in on this browser's Portal session, as `/authorize`
/// does for an app, and sends the browser to its first step.
async fn start_sign_in(
    engine: &OAuthEngine,
    cookies: Option<(String, String)>,
    now: i64,
) -> Response {
    let store = engine.server.store().clone();
    let started = blocking(move || {
        let login = cookies
            .and_then(|(cookie, csrf)| store.authority.portal_login(&cookie, &csrf, now).ok());
        let (session_id, guest) = match &login {
            Some(login) => (login.session_id.clone(), None),
            None => {
                let guest = store
                    .authority
                    .create_portal_session(now)
                    .map_err(|cause| {
                        super::http::failed(
                            "could not open a Portal session for the device page",
                            cause,
                        )
                    })?;
                (guest.session_id.clone(), Some(guest))
            }
        };
        let transaction = super::super::sqlite::id();
        let flow = super::portal::PortalAuthorization {
            presentation: None,
            session_id,
            request: super::portal::ConsentRequest::Device {
                device_code: None,
                confirmed: false,
            },
            public_jwk: serde_json::Value::Null,
            expires_at: now + 600,
        };
        store
            .store_authorization(&transaction, &flow, None)
            .map_err(|cause| super::http::failed("could not save the device sign-in", cause))?;
        Ok::<_, ErrorCode>((guest, transaction))
    })
    .await;
    match started {
        Ok((None, transaction)) => Redirect::to(&format!(
            "/device?transaction={transaction}&account=confirm"
        ))
        .into_response(),
        Ok((Some(guest), transaction)) => super::http::cookie_response(
            &guest.cookie,
            Some(&guest.csrf),
            600,
            Redirect::to(&format!("/login?transaction={transaction}")).into_response(),
        ),
        Err(code) => super::http::error(code),
    }
}

/// The page for a device sign-in's current step (see [`entry`]).
async fn step(
    engine: &OAuthEngine,
    cookies: Option<(String, String)>,
    transaction: &str,
    account: bool,
    now: i64,
) -> Response {
    let Some((cookie, csrf)) = cookies else {
        return super::http::lapsed(engine, transaction, ErrorCode::AccessDenied).await;
    };
    let store = engine.server.store().clone();
    let id = transaction.to_owned();
    let found = blocking(move || {
        let session = store
            .authority
            .portal_session_identity(&cookie, &csrf, now)
            .map_err(|cause| {
                super::http::denied_or_failed("could not read the Portal session", cause)
            })?;
        let flow = store
            .authorization(&id, &session, now)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        let person =
            match store.authority.portal_login(&cookie, &csrf, now) {
                Ok(login) => Some(signed_in(&store, &login.principal_id).map_err(|cause| {
                    super::http::failed("could not read who is signed in", cause)
                })?),
                Err(_) => None,
            };
        Ok::<_, ErrorCode>((flow, person))
    })
    .await;
    let (flow, person) = match found {
        Ok(found) => found,
        Err(code) => return super::http::lapsed(engine, transaction, code).await,
    };
    let Some(person) = person else {
        return Redirect::to(&format!("/login?transaction={transaction}")).into_response();
    };
    let device = match &flow.request {
        super::portal::ConsentRequest::Authorization(_) => {
            return super::http::error(ErrorCode::InvalidRequest)
        }
        super::portal::ConsentRequest::Device {
            device_code: None, ..
        } => serde_json::json!({
            "step": if account { "account" } else { "code" },
            "transaction": transaction,
            "signedInAs": person,
        }),
        super::portal::ConsentRequest::Device {
            confirmed: false, ..
        } => {
            let Ok(intent) = engine.consent_intent(&flow.request).await else {
                // The code expired or was used since it was entered.
                return super::http::device_code_invalid(engine);
            };
            let client = engine.server.store().get_client(&intent.client_id).await;
            let store = engine.server.store().clone();
            let client_id = intent.client_id.as_str().to_owned();
            let registered = blocking(move || {
                store.authority.read(|connection| {
                    Ok(connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients \
                        WHERE client_id=?1 AND state='active')",
                        [&client_id],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
            })
            .await;
            let (Ok(Some(client)), Ok(registered)) = (client, registered) else {
                return super::http::error(super::http::failed(
                    "could not read the device's app",
                    "its client could not be read",
                ));
            };
            let (requested_at, requested_from, device_name) =
                intent.device_requested.unwrap_or((0, None, None));
            serde_json::json!({
                "step": "confirm",
                "transaction": transaction,
                "signedInAs": person,
                "request": {
                    "clientId": intent.client_id.as_str(),
                    "clientName": client.name,
                    "registered": registered,
                    "userCode": intent.device_user_code,
                    "requestedAt": requested_at,
                    "requestedFrom": requested_from,
                    "deviceName": device_name,
                },
            })
        }
        super::portal::ConsentRequest::Device { .. } => {
            return Redirect::to(&format!("/consent?transaction={transaction}")).into_response()
        }
    };
    let theme = super::theme::portal_theme(engine, Some(transaction.to_owned())).await;
    super::assets::page(
        engine,
        None,
        Some(serde_json::json!({ "device": device, "theme": theme })),
    )
}

/// Who is signed in, as [`super::http::signed_in_as`] gives it, with the URL
/// of their profile picture when there is one, so the page can show it.
fn signed_in(
    store: &super::OAuthStorage,
    principal: &str,
) -> Result<serde_json::Value, super::super::AuthError> {
    store.authority.read(|connection| {
        let mut person = super::http::signed_in_as(connection, principal)?;
        let profile = super::super::profiles::read(connection, principal)?;
        person["avatar"] = serde_json::json!(super::super::avatars::avatar_url(
            &store.authority,
            connection,
            principal,
            &profile
        ));
        Ok(person)
    })
}

/// `POST /portal/device`: the code step. The person, signed in, enters the
/// code their device shows; a code that matches a pending request moves this
/// sign-in on to confirming the device's details. Every code that can't be
/// used gets one answer, so the page never tells valid codes from invalid ones.
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
    let (Some(code), Some(csrf), Some(transaction), Some(cookie)) = (
        form.get("user_code"),
        form.get("csrf"),
        form.get("transaction"),
        super::http::portal_cookie(&headers, "__Host-trellis-portal"),
    ) else {
        return super::http::error(ErrorCode::InvalidRequest);
    };
    if code.len() > 32 || ulid::Ulid::from_string(transaction).is_err() {
        return super::http::error(ErrorCode::InvalidRequest);
    }
    let now = crate::clock::unix_now();
    let flow =
        match super::http::login_and_flow(&engine, cookie, csrf.clone(), transaction, now).await {
            Ok((_, flow)) => flow,
            Err(code) => return super::http::error(code),
        };
    if !matches!(
        flow.request,
        super::portal::ConsentRequest::Device {
            device_code: None,
            ..
        }
    ) {
        return super::http::error(ErrorCode::InvalidRequest);
    }
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
    let id = transaction.clone();
    let entered = blocking(move || {
        let selection = store
            .device_selection(&grant.device_code)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        let next = super::portal::PortalAuthorization {
            presentation: None,
            session_id: flow.session_id.clone(),
            request: super::portal::ConsentRequest::Device {
                device_code: Some(grant.device_code),
                confirmed: false,
            },
            public_jwk: selection.public_jwk,
            expires_at: grant
                .expires_at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_secs() as i64)
                .unwrap_or(now),
        };
        store
            .advance_authorization(&id, &flow, &next, Some(grant.client_id.as_str()), now)
            .map_err(|_| ErrorCode::InvalidRequest)
    })
    .await;
    match entered {
        Ok(()) => Redirect::to(&format!("/device?transaction={transaction}")).into_response(),
        Err(code) => super::http::error(code),
    }
}

/// `POST /portal/device/confirm`: the person says the device whose details
/// they were shown is theirs (`decision=confirm`), which leads on to the
/// consent page, or cancels (`decision=cancel`), which refuses the device.
pub(super) async fn confirm(
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
    let (Some(csrf), Some(transaction), Some(decision), Some(cookie)) = (
        form.get("csrf"),
        form.get("transaction"),
        form.get("decision"),
        super::http::portal_cookie(&headers, "__Host-trellis-portal"),
    ) else {
        return super::http::error(ErrorCode::InvalidRequest);
    };
    if ulid::Ulid::from_string(transaction).is_err() {
        return super::http::error(ErrorCode::InvalidRequest);
    }
    let now = crate::clock::unix_now();
    let flow =
        match super::http::login_and_flow(&engine, cookie, csrf.clone(), transaction, now).await {
            Ok((_, flow)) => flow,
            Err(code) => return super::http::error(code),
        };
    let super::portal::ConsentRequest::Device {
        device_code: Some(device_code),
        confirmed: false,
    } = &flow.request
    else {
        return super::http::error(ErrorCode::InvalidRequest);
    };
    let device = match engine.server.store().get_device_grant(device_code).await {
        Ok(Some(device)) => device,
        _ => return super::http::error(ErrorCode::InvalidRequest),
    };
    let store = engine.server.store().clone();
    let id = transaction.clone();
    match decision.as_str() {
        "confirm" => {
            let next = super::portal::PortalAuthorization {
                presentation: None,
                session_id: flow.session_id.clone(),
                request: super::portal::ConsentRequest::Device {
                    device_code: Some(device.device_code.clone()),
                    confirmed: true,
                },
                public_jwk: flow.public_jwk.clone(),
                expires_at: flow.expires_at,
            };
            let client = device.client_id.as_str().to_owned();
            match blocking(move || {
                store
                    .advance_authorization(&id, &flow, &next, Some(&client), now)
                    .map_err(|_| ErrorCode::InvalidRequest)
            })
            .await
            {
                Ok(()) => {
                    Redirect::to(&format!("/consent?transaction={transaction}")).into_response()
                }
                Err(code) => super::http::error(code),
            }
        }
        "cancel" => {
            let session = flow.session_id.clone();
            if blocking(move || {
                store
                    .take_authorization(&id, &session, now)
                    .map_err(|_| ErrorCode::InvalidRequest)
            })
            .await
            .is_err()
            {
                return super::http::error(ErrorCode::InvalidRequest);
            }
            match engine.server.deny_device(&device.user_code).await {
                Ok(()) => Redirect::to("/device/complete?decision=denied").into_response(),
                Err(_) => super::http::error(ErrorCode::InvalidRequest),
            }
        }
        _ => super::http::error(ErrorCode::InvalidRequest),
    }
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
        requested_from: Option<std::net::IpAddr>,
        device_name: Option<String>,
    ) -> Result<DeviceAuthorizationResponse, ErrorResponse> {
        let requested = RequestedAuthority::parse(details)?;
        self.prepare_client(client_id).await?;
        let authority = self.server.store().authority.clone();
        let (client, selection) = (
            client_id.as_str().to_owned(),
            requested.capabilities.clone(),
        );
        let kind = blocking(move || {
            authority.transaction(|connection| {
                let kind = connection.query_row(
                    "SELECT kind FROM auth_oauth_clients WHERE client_id=?1 \
                    UNION ALL SELECT kind FROM auth_cimd_clients WHERE client_id=?1 \
                    AND NOT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1)",
                    [&client],
                    |row| row.get::<_, String>(0),
                )?;
                if kind == "native" {
                    super::enrollment::validate_selection(
                        connection,
                        &client,
                        &selection.required,
                        &selection.optional,
                    )?;
                }
                Ok(kind)
            })
        })
        .await
        .map_err(|_| ErrorResponse::new(ErrorCode::InvalidRequest))?;
        // Device Code is the native flow: a browser client would otherwise fail
        // later as a bare unauthorized client, hiding the registration mistake.
        if kind != "native" {
            return Err(
                ErrorResponse::new(ErrorCode::UnauthorizedClient).with_description(format!(
                    "Device Code sign-in needs a native app client, but client '{}' is \
                    registered as a {kind} app",
                    client_id.as_str()
                )),
            );
        }
        let uri = format!(
            "{}/device_authorization",
            self.server.store().authority.settings.oauth_issuer
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
            .map_err(|cause| {
                ErrorResponse::new(super::http::failed(
                    "could not record the device request's DPoP proof",
                    cause,
                ))
            })?
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
            .map_err(|cause| {
                ErrorResponse::new(super::http::failed(
                    "could not read the new device grant",
                    cause,
                ))
            })?
            .ok_or_else(|| {
                ErrorResponse::new(super::http::failed(
                    "could not read the new device grant",
                    "it was not stored",
                ))
            })?;
        let store = self.server.store().clone();
        let selection = DeviceSelection {
            client_id: client_id.as_str().into(),
            authorization_details: details.into(),
            persistent_dpop_jkt: binding.persistent_dpop_jkt,
            public_jwk: binding.public_jwk,
            requested_from: requested_from.map(|address| address.to_string()),
            device_name,
        };
        blocking(move || store.store_device_selection(&grant, &selection))
            .await
            .map_err(|cause| {
                ErrorResponse::new(super::http::failed(
                    "could not save the device grant's capabilities",
                    cause,
                ))
            })?;
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
            .map_err(|cause| {
                ErrorResponse::new(super::http::failed(
                    "could not look up the device code",
                    cause,
                ))
            })?
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

#[cfg(test)]
mod tests {
    use super::device_name;

    /// A reported name cannot break the page's layout or reorder what it
    /// shows, and cannot fill it: control characters and bidirectional
    /// marks go, whitespace collapses, and the name is cut to its limit.
    #[test]
    fn a_reported_device_name_is_cleaned_and_bounded() {
        assert_eq!(
            device_name("  build\tbox\r\n7\u{0}  ").as_deref(),
            Some("build box 7")
        );
        assert_eq!(
            device_name("laptop\u{202e}txt.exe").as_deref(),
            Some("laptop txt.exe")
        );
        assert_eq!(device_name("\u{7}\n \u{2066}"), None);
        let long = "é".repeat(200);
        assert_eq!(
            device_name(&long).map(|name| name.chars().count()),
            Some(64)
        );
    }
}
