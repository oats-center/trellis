//! Exact broker effects using the runtime's operational system-account client.

use std::time::Duration;

use async_nats::{jetstream::kv, Client};
use serde::Deserialize;

use super::sqlite::{json, AuthError, SqliteAuthorizationStore};

pub(crate) const ATTACHMENT_MARKER_PREFIX: &str = "trellis.auth.v1:";

#[derive(Deserialize)]
struct ServerIdentity {
    id: String,
}
#[derive(Deserialize)]
struct ConnzReply {
    server: ServerIdentity,
    data: ConnzData,
}
#[derive(Deserialize)]
struct ConnzData {
    server_id: String,
    num_connections: usize,
    connections: Vec<ConnzConnection>,
}
#[derive(Deserialize)]
struct ConnzConnection {
    cid: u64,
    authorized_user: Option<String>,
}

/// Never kick a reused numeric CID belonging to a different attachment. Missing
/// or malformed system inventory is retryable, not proof that a socket is gone.
pub(crate) async fn close_attachment(
    client: &Client,
    server: &str,
    cid: u64,
    attachment: &str,
) -> Result<(), AuthError> {
    if server.is_empty() || server.contains(['.', ' ', '*', '>']) {
        return Err(AuthError::Denied);
    }
    let inventory = tokio::time::timeout(
        Duration::from_secs(5),
        client.request(
            format!("$SYS.REQ.SERVER.{server}.CONNZ"),
            serde_json::to_vec(&serde_json::json!({"auth":true,"cid":cid,"limit":1}))?.into(),
        ),
    )
    .await
    .map_err(|_| AuthError::Unavailable)?
    .map_err(|error| AuthError::Broker(error.to_string()))?;
    let reply: ConnzReply = serde_json::from_slice(&inventory.payload)?;
    if reply.server.id != server || reply.data.server_id != server {
        return Err(AuthError::Unavailable);
    }
    let target = reply
        .data
        .connections
        .iter()
        .find(|connection| connection.cid == cid);
    let Some(target) = target else {
        // CONNZ `total` counts all sockets, even with a CID filter. The
        // filtered count is the evidence that this exact socket is absent.
        if reply.data.num_connections == 0 && reply.data.connections.is_empty() {
            return Ok(());
        }
        return Err(AuthError::Unavailable);
    };
    let marker = target
        .authorized_user
        .as_ref()
        .ok_or(AuthError::Unavailable)?;
    if marker != &format!("{ATTACHMENT_MARKER_PREFIX}{attachment}") {
        return Ok(());
    }
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client.request(
            format!("$SYS.REQ.SERVER.{server}.KICK"),
            serde_json::to_vec(&serde_json::json!({"cid":cid}))?.into(),
        ),
    )
    .await
    .map_err(|_| AuthError::Unavailable)?
    .map_err(|error| AuthError::Broker(error.to_string()))?;
    let reply: serde_json::Value = serde_json::from_slice(&response.payload)?;
    if reply
        .pointer("/server/id")
        .and_then(serde_json::Value::as_str)
        != Some(server)
        || reply.get("error").is_some()
    {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

impl SqliteAuthorizationStore {
    /// One ordinary worker turn: resume one enforcement page and a bounded
    /// outbox batch. Publication acknowledges durable JetStream acceptance;
    /// physical closure completes only after exact system-account evidence.
    pub(crate) async fn process_effects(
        &self,
        material: &kv::Store,
        revocations: &kv::Store,
        system: &Client,
        now: i64,
    ) -> Result<usize, AuthError> {
        self.enforce_next_page(now)?;
        let effects = self.claim_effects(8, now, 10)?;
        let mut completed = 0;
        let mut failure = None;
        for effect in effects {
            let result = async {
                match effect.kind.as_str() {
                    "authority_publish" => {
                        let kind = effect.payload["kind"].as_str().ok_or(AuthError::Denied)?;
                        let key = if kind == "rotation" {
                            effect.payload["sequence"]
                                .as_u64()
                                .ok_or(AuthError::Denied)?
                                .to_string()
                        } else {
                            effect.payload["digest"]
                                .as_str()
                                .ok_or(AuthError::Denied)?
                                .to_owned()
                        };
                        let bytes = self.resolve_verification_material(kind, &key)?;
                        material
                            .put(format!("{kind}.{key}"), bytes.into())
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
                    }
                    "session_revoke" => {
                        let session = effect.payload["authorizationSessionId"]
                            .as_str()
                            .ok_or(AuthError::Denied)?;
                        let signed = self
                            .resolve_revocation(session)?
                            .ok_or(AuthError::NotFound)?;
                        if signed.latest_context_expiry > now {
                            let ttl = Duration::from_secs(
                                u64::try_from(signed.latest_context_expiry - now)
                                    .map_err(|_| AuthError::Denied)?,
                            );
                            let status = revocations
                                .status()
                                .await
                                .map_err(|error| AuthError::Broker(error.to_string()))?;
                            // Earlier eviction would violate the live-context
                            // denial promise, regardless of the configured TTL.
                            if !status.max_age().is_zero() && status.max_age() < ttl {
                                return Err(AuthError::Unavailable);
                            }
                            let bytes = json(&signed)?.into_bytes();
                            if let Err(error) = revocations
                                .create_with_ttl(session, bytes.clone().into(), ttl)
                                .await
                            {
                                let existing = revocations
                                    .get(session)
                                    .await
                                    .map_err(|error| AuthError::Broker(error.to_string()))?;
                                if existing.as_deref() != Some(bytes.as_slice()) {
                                    return Err(AuthError::Broker(error.to_string()));
                                }
                            }
                        }
                    }
                    "kick" => {
                        close_attachment(
                            system,
                            effect.payload["serverId"]
                                .as_str()
                                .ok_or(AuthError::Denied)?,
                            effect.payload["brokerClientId"]
                                .as_u64()
                                .ok_or(AuthError::Denied)?,
                            effect.payload["attachmentId"]
                                .as_str()
                                .ok_or(AuthError::Denied)?,
                        )
                        .await?;
                    }
                    _ => return Err(AuthError::Invalid("unknown committed effect".into())),
                }
                self.complete_effect(&effect, now)
            }
            .await;
            match result {
                Ok(()) => completed += 1,
                Err(error) => {
                    tracing::warn!(action_id=%effect.action_id,kind=%effect.kind,attempt=effect.attempts,error=%error,"Auth effect remains pending");
                    self.retry_effect(&effect, now)?;
                    failure = Some(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }
}
