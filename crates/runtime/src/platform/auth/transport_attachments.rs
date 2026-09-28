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
    server: ServerIdentity,
    data: ConnzData,
}

/// Broker `CONNZ` payload, nested under the system reply envelope's `data`.
#[derive(Debug, serde::Deserialize)]
struct ConnzData {
    server_id: String,
    total: usize,
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
    loop {
        match tokio::time::timeout_at(deadline, subscription.next()).await {
            Ok(Some(message)) => {
                if let Ok(reply) = serde_json::from_slice::<StatszReply>(&message.payload) {
                    if !reply.server.id.is_empty() {
                        servers.insert(reply.server.id);
                    }
                }
            }
            _ => break,
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
        AuthorizationStateError::Storage(format!(
            "CONNZ reply is not a valid server system envelope: {error}"
        ))
    })?;
    if reply.server.id != server_id || reply.data.server_id != server_id {
        return Err(AuthorizationStateError::Storage(format!(
            "CONNZ reply identified server {} (envelope {}) but {} was requested",
            reply.data.server_id, reply.server.id, server_id
        )));
    }
    let connections = reply
        .data
        .connections
        .into_iter()
        .map(|connection| BrokerConnection {
            cid: connection.cid,
            authorized_user: connection.authorized_user,
        })
        .collect();
    Ok((connections, reply.data.total))
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
    fn connz_payload_without_the_server_envelope_is_not_an_empty_inventory() {
        // A broker reply that omits the `{server, data}` envelope (or its
        // server identity) must fail closed rather than decode as an empty
        // inventory, which would falsely prove a live attachment absent.
        let legacy_top_level = br#"{
            "server_id": "N1",
            "total": 1,
            "connections": [{ "cid": 7, "authorized_user": "trellis.auth.v1:digest:N1:7" }]
        }"#;
        assert!(
            serde_json::from_slice::<ConnzReply>(legacy_top_level).is_err(),
            "a reply without the system envelope must not decode"
        );
        let missing_server_identity = br#"{
            "server": { "name": "trellis" },
            "data": { "server_id": "N1", "total": 0, "connections": [] }
        }"#;
        assert!(
            serde_json::from_slice::<ConnzReply>(missing_server_identity).is_err(),
            "a reply without a server identity must not decode"
        );
        let missing_connections = br#"{
            "server": { "id": "N1" },
            "data": { "server_id": "N1", "total": 0 }
        }"#;
        assert!(
            serde_json::from_slice::<ConnzReply>(missing_connections).is_err(),
            "a reply without a connection array must not decode as an empty inventory"
        );
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

    /// Resolve the pinned NATS binary, preferring the shared cache so a prepared
    /// machine never needs a network fetch to run the live broker test.
    #[cfg(feature = "nats-leases")]
    fn pinned_nats_binary() -> std::path::PathBuf {
        use trellis_local_nats::{NatsBinarySource, NatsServerBinary};
        let name = format!(
            "nats-server-v{}",
            trellis_local_nats::pinned_version().expect("pinned nats version")
        );
        if let Some(home) = std::env::var_os("HOME") {
            let candidate = std::path::PathBuf::from(home)
                .join(".cache/trellis")
                .join(&name);
            if candidate.is_file() {
                if let Ok(path) =
                    NatsServerBinary::resolve(&NatsBinarySource::Path(candidate), None)
                {
                    return path;
                }
            }
        }
        let cache = std::env::temp_dir().join("trellis-connz-nats-cache");
        std::fs::create_dir_all(&cache).expect("private nats cache dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))
                .expect("private nats cache permissions");
        }
        NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, Some(&cache))
            .expect("resolve pinned nats-server")
    }

    #[cfg(feature = "nats-leases")]
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local addr")
            .port()
    }

    /// A real broker with a system account answers the production `CONNZ`
    /// inventory path. The wrapped reply must identify the requested server and
    /// expose a known authenticated socket, and unavailable evidence must fail
    /// closed instead of reading as an empty inventory.
    #[cfg(feature = "nats-leases")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connz_finds_a_known_socket_and_fails_closed_on_unavailable_evidence() {
        use trellis_local_nats::{ManagedNatsServer, NatsOutput};

        let binary = pinned_nats_binary();
        let dir = tempfile::tempdir().expect("temp dir");
        let config_path = dir.path().join("nats.conf");
        let nats_port = free_port();
        let http_port = free_port();
        let ws_port = free_port();
        let config = format!(
            "port: {nats_port}\nhttp_port: {http_port}\n\
             websocket {{ port: {ws_port}, no_tls: true }}\n\
             accounts {{\n  SYS {{ users: [ {{ user: \"sys\", password: \"pw\" }} ] }}\n  APP {{ users: [ {{ user: \"app\", password: \"pw\" }} ] }}\n}}\n\
             system_account: SYS\n"
        );
        std::fs::write(&config_path, config).expect("write broker config");
        let mut server = ManagedNatsServer::start(
            &binary,
            &config_path,
            nats_port,
            http_port,
            ws_port,
            &dir.path().join("nats.pid"),
            &NatsOutput::Log {
                path: dir.path().join("nats.log"),
                mirror: false,
            },
        )
        .expect("start broker");
        let url = format!("nats://127.0.0.1:{nats_port}");

        let system =
            async_nats::ConnectOptions::with_user_and_password("sys".to_owned(), "pw".to_owned())
                .connect(&url)
                .await
                .expect("connect system account");
        // Held open for the duration so the broker inventories a real socket.
        let _app =
            async_nats::ConnectOptions::with_user_and_password("app".to_owned(), "pw".to_owned())
                .connect(&url)
                .await
                .expect("connect app account");

        let servers = discover_servers(&system).await.expect("discover servers");
        assert_eq!(servers.len(), 1, "exactly one broker answers STATSZ");
        let server_id = servers.into_iter().next().expect("server identity");

        let inventory = paginate_connz(&system, &server_id)
            .await
            .expect("read CONNZ inventory");
        let app_connection = inventory
            .iter()
            .find(|connection| connection.authorized_user.as_deref() == Some("app"))
            .expect("the authenticated app socket must be inventoried");
        assert!(app_connection.cid > 0);

        let (filtered, total) = connz(&system, &server_id, 0, Some(app_connection.cid))
            .await
            .expect("read one connection");
        assert!(total >= 1);
        assert_eq!(filtered.len(), 1, "a cid filter returns exactly one socket");
        assert_eq!(filtered[0].cid, app_connection.cid);
        assert_eq!(filtered[0].authorized_user.as_deref(), Some("app"));

        // An unknown server has no responder. That must be an error, never a
        // successful empty inventory that would prove a live attachment absent.
        let unknown = connz(&system, "UNKNOWN_SERVER_IDENTITY", 0, None).await;
        assert!(
            unknown.is_err(),
            "unavailable inventory evidence must fail closed: {unknown:?}"
        );

        server.stop().expect("stop broker");
    }
}
