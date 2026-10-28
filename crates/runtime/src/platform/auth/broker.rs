//! Exact broker effects using the runtime's operational system-account client.

use std::{sync::Arc, time::Duration};

use async_nats::{jetstream::kv, Client};
use serde::Deserialize;

use super::sqlite::{blocking, json, AuthError, SqliteAuthorizationStore};

pub(crate) const ATTACHMENT_MARKER_PREFIX: &str = "trellis.auth.v1:";
pub(crate) const MATERIAL_BUCKET: &str = "TRELLIS_AUTH_MATERIAL";
pub(crate) const REVOCATION_BUCKET: &str = "TRELLIS_AUTH_REVOCATIONS";

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
    /// Publish committed immutable verification material and await its broker acknowledgment.
    pub(crate) async fn publish_verification_material(
        self: &Arc<Self>,
        material: &kv::Store,
        kind: &str,
        key: &str,
    ) -> Result<(), AuthError> {
        let prefix = match kind {
            "authority" => "context",
            "catalog" => "api",
            "provider" => "provider",
            "rotation" => "rotation",
            _ => return Err(AuthError::Invalid("unknown verification material".into())),
        };
        let store = Arc::clone(self);
        let (selected_kind, selected_key) = (kind.to_owned(), key.to_owned());
        let bytes =
            blocking(move || store.resolve_verification_material(&selected_kind, &selected_key))
                .await?;
        material
            .put(format!("{prefix}.{key}"), bytes.into())
            .await
            .map_err(|error| AuthError::Broker(error.to_string()))?;
        Ok(())
    }

    /// One ordinary worker turn: resume one enforcement page and a bounded
    /// outbox batch. Publication acknowledges durable JetStream acceptance;
    /// physical closure completes only after exact system-account evidence.
    /// Advance one bounded enforcement page and deliver committed broker effects.
    /// Count progress in both phases so drains cannot stop at effect-free pages.
    pub(crate) async fn process_effects(
        self: &Arc<Self>,
        material: &kv::Store,
        revocations: &kv::Store,
        system: &Client,
        now: i64,
    ) -> Result<usize, AuthError> {
        let started = std::time::Instant::now();
        let store = Arc::clone(self);
        let (enforcement_progress, effects) = blocking(move || {
            let progress = store.enforce_next_page(now)?.is_some();
            Ok::<_, AuthError>((progress, store.claim_effects(8, now, 10, false)?))
        })
        .await?;
        // An empty enforcement page still advances durable work. In particular,
        // startup must drain bootstrap scopes before accepting policy mutations.
        let mut completed = usize::from(enforcement_progress);
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
                        self.publish_verification_material(material, kind, &key)
                            .await?;
                        Ok(())
                    }
                    "session_revoke" => {
                        let session = effect.payload["authorizationSessionId"]
                            .as_str()
                            .ok_or(AuthError::Denied)?;
                        let store = Arc::clone(self);
                        let lookup = session.to_owned();
                        let signed = blocking(move || store.resolve_revocation(&lookup))
                            .await?
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
                        Ok(())
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
                        .await
                    }
                    _ => Err(AuthError::Invalid("unknown committed effect".into())),
                }
            }
            .await;
            // Broker waits consume this batch's claim lifetime, too.
            let settled_at =
                now.saturating_add(i64::try_from(started.elapsed().as_secs()).unwrap_or(i64::MAX));
            match result {
                Ok(()) => {
                    let store = Arc::clone(self);
                    completed += usize::from(
                        blocking(move || store.complete_effect(&effect, settled_at)).await?,
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        action_id=%effect.action_id,
                        kind=%effect.kind,
                        attempt=effect.attempts,
                        error=%error,
                        "Auth effect remains pending"
                    );
                    let store = Arc::clone(self);
                    blocking(move || store.retry_effect(&effect, settled_at)).await?;
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
