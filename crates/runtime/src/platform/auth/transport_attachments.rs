//! Broker-inventory reconciliation for retained physical attachments.
//!
//! This module owns the comparison and inventory operations the Auth Callout
//! lifecycle uses to converge retained attachment records with the broker:
//! discovering responsive servers, paging authenticated connection inventory,
//! proving absence of a single attachment, and requesting a kick. It is not a
//! separate service.

use std::collections::BTreeSet;
use std::time::Duration;

use async_nats::Client;
use bytes::Bytes;
use futures_util::StreamExt;

use serde::{Deserialize, Serialize};

use super::context::AuthorizationContextSelector;
use super::{
    validate_connection_kick_response, AuthorizationStateError, ConnectionKickOutcome,
    GrantOwnerKind, PostCommitActionRecord,
};

/// Marker prefix installed by the Auth Callout as the authenticated NATS user
/// name. Only callout-owned sockets carry it.
pub(crate) const ATTACHMENT_MARKER_PREFIX: &str = "trellis.auth.v1:";

const STATSZ_SUBJECT: &str = "$SYS.REQ.SERVER.PING.STATSZ";
const CONNZ_PAGE_LIMIT: usize = 256;
const STATSZ_WINDOW: Duration = Duration::from_millis(750);
const BROKER_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Format of a durable `TransportReevaluate` post-commit payload.
pub(crate) const TRANSPORT_REEVALUATE_FORMAT_V1: &str = "trellis.transport-reevaluate-action.v1";

/// Format of a durable explicit physical-attachment kick payload.
pub(crate) const PHYSICAL_ATTACHMENT_KICK_FORMAT_V1: &str =
    "trellis.physical-attachment-kick-action.v1";

/// Exact physical attachment target for an administrative kick.
///
/// Unlike a scope selector, this names one broker attachment unambiguously:
/// the server, the numeric client id, and the physical connection id must all
/// agree before the kick is applied.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PhysicalAttachmentTarget {
    pub server_id: String,
    pub client_id: String,
    pub physical_connection_id: String,
}

/// Builds an explicit physical-attachment target from a retained record.
pub(crate) fn physical_attachment_target(
    connection: &super::AuthConnectionPresence,
) -> PhysicalAttachmentTarget {
    PhysicalAttachmentTarget {
        server_id: connection.server_id.clone(),
        client_id: connection.client_id.clone(),
        physical_connection_id: connection.connection_id.clone(),
    }
}

/// One typed scope whose physical attachments must be re-evaluated.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "scope")]
pub(crate) enum TransportReevaluateScope {
    /// One logical runtime connection.
    RuntimeConnection { runtime_connection_id: String },
    /// One login session's contexts.
    Login { login_session_id: String },
    /// Every context of one principal.
    Principal { principal_id: String },
    /// Every context of one participant within its owner.
    Participant { participant_id: String },
    /// One owner/participant grant.
    Grant {
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
    },
    /// One deployment's contexts.
    Deployment { deployment_id: String },
    /// One runtime instance's contexts.
    Instance { instance_id: String },
    /// Every context signed by one issuer.
    Issuer { issuer_key_id: String },
}

impl TransportReevaluateScope {
    /// Builds the reevaluation scope matching a context selector.
    pub(crate) fn from_context_selector(selector: &AuthorizationContextSelector) -> Self {
        match selector {
            AuthorizationContextSelector::Login(id) => Self::Login {
                login_session_id: id.clone(),
            },
            AuthorizationContextSelector::RuntimeConnection(id) => Self::RuntimeConnection {
                runtime_connection_id: id.clone(),
            },
            AuthorizationContextSelector::Principal(id) => Self::Principal {
                principal_id: id.clone(),
            },
            AuthorizationContextSelector::Participant(id) => Self::Participant {
                participant_id: id.clone(),
            },
            AuthorizationContextSelector::Grant(kind, owner_id, participant_id) => Self::Grant {
                owner_kind: *kind,
                owner_id: owner_id.clone(),
                participant_id: participant_id.clone(),
            },
            AuthorizationContextSelector::Deployment(id) => Self::Deployment {
                deployment_id: id.clone(),
            },
            AuthorizationContextSelector::Instance(id) => Self::Instance {
                instance_id: id.clone(),
            },
            AuthorizationContextSelector::Issuer(id) => Self::Issuer {
                issuer_key_id: id.clone(),
            },
        }
    }

    /// Selects the durable contexts admitted in this scope.
    pub(crate) fn to_context_selector(&self) -> AuthorizationContextSelector {
        match self {
            Self::RuntimeConnection {
                runtime_connection_id,
            } => AuthorizationContextSelector::RuntimeConnection(runtime_connection_id.clone()),
            Self::Login { login_session_id } => {
                AuthorizationContextSelector::Login(login_session_id.clone())
            }
            Self::Principal { principal_id } => {
                AuthorizationContextSelector::Principal(principal_id.clone())
            }
            Self::Participant { participant_id } => {
                AuthorizationContextSelector::Participant(participant_id.clone())
            }
            Self::Grant {
                owner_kind,
                owner_id,
                participant_id,
            } => AuthorizationContextSelector::Grant(
                *owner_kind,
                owner_id.clone(),
                participant_id.clone(),
            ),
            Self::Deployment { deployment_id } => {
                AuthorizationContextSelector::Deployment(deployment_id.clone())
            }
            Self::Instance { instance_id } => {
                AuthorizationContextSelector::Instance(instance_id.clone())
            }
            Self::Issuer { issuer_key_id } => {
                AuthorizationContextSelector::Issuer(issuer_key_id.clone())
            }
        }
    }
}

/// Canonical payload of a transport-policy reevaluation action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TransportReevaluatePayload {
    pub format: String,
    pub scope: TransportReevaluateScope,
}

/// Builds one deterministic reevaluation action for a scope.
///
/// The action carries no frozen allowlist: the worker resolves present state
/// when it runs, so coalescing equal scopes within one change is safe. `token`
/// disambiguates distinct mutations of the same scope so a later change is not
/// mistaken for a replay of an earlier one.
pub(crate) fn transport_reevaluate_action(
    scope: &TransportReevaluateScope,
    at_ms: i64,
    token: &str,
) -> Result<PostCommitActionRecord, AuthorizationStateError> {
    let payload = serde_json::to_value(TransportReevaluatePayload {
        format: TRANSPORT_REEVALUATE_FORMAT_V1.to_owned(),
        scope: scope.clone(),
    })
    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    let action_id = trellis_protocol::digest_json(&serde_json::json!({
        "transportReevaluate": scope,
        "at": at_ms,
        "token": token,
    }))
    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok(PostCommitActionRecord {
        predecessor_action_id: None,
        action_id,
        kind: super::PostCommitActionKind::TransportReevaluate,
        payload,
        created_at: at_ms,
        attempts: 0,
        next_attempt_at: at_ms,
        claimed_until: None,
        last_error: None,
    })
}

/// One broker-reported connection, as returned by authenticated `CONNZ`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrokerConnection {
    pub cid: u64,
    /// The broker's authenticated user field (`authorized_user`), never the
    /// freely chosen connection name.
    pub authorized_user: Option<String>,
}

impl BrokerConnection {
    /// Whether this socket was admitted by the Trellis Auth Callout.
    pub(crate) fn is_callout_owned(&self) -> bool {
        self.authorized_user
            .as_deref()
            .is_some_and(|user| user.starts_with(ATTACHMENT_MARKER_PREFIX))
    }
}

#[derive(Debug, serde::Deserialize)]
struct StatszReply {
    server: ServerIdentity,
}

#[derive(Debug, serde::Deserialize)]
struct ServerIdentity {
    id: String,
}

#[derive(Debug, serde::Deserialize)]
struct ConnzReply {
    #[serde(default)]
    server_id: String,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    connections: Vec<ConnzConnection>,
}

#[derive(Debug, serde::Deserialize)]
struct ConnzConnection {
    cid: u64,
    #[serde(default)]
    authorized_user: Option<String>,
}

/// Discovers responsive server IDs by scattering `STATSZ` over a fresh inbox.
pub(crate) async fn discover_servers(
    client: &Client,
) -> Result<Vec<String>, AuthorizationStateError> {
    let inbox = client.new_inbox();
    let mut subscription = client
        .subscribe(inbox.clone())
        .await
        .map_err(|error| storage(format!("failed to subscribe for STATSZ replies: {error}")))?;
    client
        .publish_with_reply(STATSZ_SUBJECT, inbox, Bytes::new())
        .await
        .map_err(|error| storage(format!("failed to publish STATSZ request: {error}")))?;
    let mut servers = BTreeSet::new();
    let deadline = tokio::time::Instant::now() + STATSZ_WINDOW;
    while let Ok(Some(message)) = tokio::time::timeout_at(deadline, subscription.next()).await {
        if let Ok(reply) = serde_json::from_slice::<StatszReply>(&message.payload) {
            if !reply.server.id.is_empty() {
                servers.insert(reply.server.id);
            }
        }
    }
    Ok(servers.into_iter().collect())
}

/// Pages authenticated `CONNZ` inventory for one server.
pub(crate) async fn paginate_connz(
    client: &Client,
    server_id: &str,
) -> Result<Vec<BrokerConnection>, AuthorizationStateError> {
    let mut offset = 0usize;
    let mut connections = Vec::new();
    loop {
        let (page, total) = connz(client, server_id, offset, None).await?;
        let page_len = page.len();
        connections.extend(page);
        offset += page_len;
        if page_len == 0 || offset >= total {
            break;
        }
    }
    Ok(connections)
}

/// Reads authenticated `CONNZ` inventory, optionally for one client id.
///
/// The reply must carry the expected server id; a mismatched or malformed reply
/// is treated as unavailable evidence rather than as an empty inventory.
pub(crate) async fn connz(
    client: &Client,
    server_id: &str,
    offset: usize,
    cid: Option<u64>,
) -> Result<(Vec<BrokerConnection>, usize), AuthorizationStateError> {
    let mut payload = serde_json::json!({
        "auth": true,
        "offset": offset,
        "limit": CONNZ_PAGE_LIMIT,
    });
    if let Some(cid) = cid {
        payload["cid"] = serde_json::json!(cid);
    }
    let payload = serde_json::to_vec(&payload)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    let response = request(
        client,
        format!("$SYS.REQ.SERVER.{server_id}.CONNZ"),
        payload,
    )
    .await?;
    let reply: ConnzReply = serde_json::from_slice(&response).map_err(|error| {
        AuthorizationStateError::Storage(format!("CONNZ reply is not valid JSON: {error}"))
    })?;
    if reply.server_id != server_id {
        return Err(AuthorizationStateError::Storage(format!(
            "CONNZ reply came from server {} but {} was requested",
            reply.server_id, server_id
        )));
    }
    let connections = reply
        .connections
        .into_iter()
        .map(|connection| BrokerConnection {
            cid: connection.cid,
            authorized_user: connection.authorized_user,
        })
        .collect();
    Ok((connections, reply.total))
}

/// Requests a connection kick and validates the broker's response.
pub(crate) async fn request_kick(
    client: &Client,
    server_id: &str,
    cid: u64,
) -> Result<ConnectionKickOutcome, AuthorizationStateError> {
    let payload = serde_json::to_vec(&serde_json::json!({ "cid": cid }))
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    let response = request(client, format!("$SYS.REQ.SERVER.{server_id}.KICK"), payload).await?;
    validate_connection_kick_response(&response, server_id)
}

/// Subject suffix of the best-effort authorization-change hint, appended to the
/// server-issued recipient inbox prefix.
pub(crate) const AUTHORIZATION_CHANGE_SUBJECT_SUFFIX: &str = "._trellis.authorization";

/// Payload format tag of the best-effort authorization-change hint.
pub(crate) const AUTHORIZATION_CHANGE_FORMAT_V1: &str = "trellis.authorization-change.v1";

/// Publish the best-effort authorization-change hint to one recipient inbox.
///
/// A hint only prompts the recipient to schedule a normal signed refresh and
/// cannot install grants, change admitted transport, or suppress revocation.
/// Callers therefore treat any failure as non-fatal: a lost hint delays
/// notification and never weakens enforcement.
pub(crate) async fn publish_authorization_hint(
    client: &Client,
    inbox_prefix: &str,
) -> Result<(), AuthorizationStateError> {
    let subject = format!("{inbox_prefix}{AUTHORIZATION_CHANGE_SUBJECT_SUFFIX}");
    let payload = Bytes::from(
        serde_json::to_vec(&serde_json::json!({ "format": AUTHORIZATION_CHANGE_FORMAT_V1 }))
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
    );
    client
        .publish(subject, payload)
        .await
        .map_err(|error| storage(format!("authorization-change hint publish failed: {error}")))
}

async fn request(
    client: &Client,
    subject: String,
    payload: Vec<u8>,
) -> Result<Vec<u8>, AuthorizationStateError> {
    let response = tokio::time::timeout(
        BROKER_REQUEST_TIMEOUT,
        client.request(subject, Bytes::from(payload)),
    )
    .await
    .map_err(|_| AuthorizationStateError::Storage("NATS system request timed out".to_owned()))?
    .map_err(|error| storage(format!("NATS system request failed: {error}")))?;
    Ok(response.payload.to_vec())
}

fn storage(message: impl std::fmt::Display) -> AuthorizationStateError {
    AuthorizationStateError::Storage(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connz_inventory_decodes_the_authenticated_user() {
        let reply: ConnzReply = serde_json::from_slice(
            br#"{
                "server_id": "N1",
                "total": 1,
                "offset": 0,
                "limit": 256,
                "connections": [
                    {
                        "cid": 7,
                        "name": "client-chosen",
                        "authorized_user": "trellis.auth.v1:digest:N1:7"
                    }
                ]
            }"#,
        )
        .expect("decode captured CONNZ inventory");
        assert_eq!(reply.server_id, "N1");
        assert_eq!(reply.total, 1);
        let connection = BrokerConnection {
            cid: reply.connections[0].cid,
            authorized_user: reply.connections[0].authorized_user.clone(),
        };
        assert_eq!(connection.cid, 7);
        assert_eq!(
            connection.authorized_user.as_deref(),
            Some("trellis.auth.v1:digest:N1:7")
        );
        assert!(connection.is_callout_owned());
    }

    #[test]
    fn only_marker_authenticated_sockets_are_callout_owned() {
        assert!(!BrokerConnection {
            cid: 1,
            authorized_user: Some("some-user".to_owned()),
        }
        .is_callout_owned());
        assert!(!BrokerConnection {
            cid: 2,
            authorized_user: None,
        }
        .is_callout_owned());
        assert!(BrokerConnection {
            cid: 3,
            authorized_user: Some("trellis.auth.v1:digest:N1:3".to_owned()),
        }
        .is_callout_owned());
    }

    #[test]
    fn reevaluate_scope_round_trips_context_selectors_and_json() {
        let selectors = [
            AuthorizationContextSelector::RuntimeConnection("conn".to_owned()),
            AuthorizationContextSelector::Login("login".to_owned()),
            AuthorizationContextSelector::Principal("principal".to_owned()),
            AuthorizationContextSelector::Participant("participant".to_owned()),
            AuthorizationContextSelector::Grant(
                GrantOwnerKind::User,
                "owner".to_owned(),
                "participant".to_owned(),
            ),
            AuthorizationContextSelector::Deployment("deployment".to_owned()),
            AuthorizationContextSelector::Instance("instance".to_owned()),
            AuthorizationContextSelector::Issuer("issuer".to_owned()),
        ];
        for selector in selectors {
            let scope = TransportReevaluateScope::from_context_selector(&selector);
            assert_eq!(
                TransportReevaluateScope::from_context_selector(&scope.to_context_selector()),
                scope
            );
            let payload = TransportReevaluatePayload {
                format: TRANSPORT_REEVALUATE_FORMAT_V1.to_owned(),
                scope,
            };
            let encoded = serde_json::to_string(&payload).expect("encode reevaluation payload");
            let decoded: TransportReevaluatePayload =
                serde_json::from_str(&encoded).expect("decode reevaluation payload");
            assert_eq!(decoded, payload);
        }
    }
}
