//! Exact broker effects using the runtime's operational system-account client.

use std::{sync::Arc, time::Duration};

use async_nats::{jetstream::kv, Client};
use futures_util::StreamExt;
use serde::Deserialize;

use super::sqlite::{blocking, json, AuthError, SqliteAuthorizationStore};
use crate::storage::StorageFailure;

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
/// True when at least one NATS server identifies itself and none is `server`.
async fn server_is_gone(client: &Client, server: &str) -> Result<bool, AuthError> {
    let inbox = client.new_inbox();
    let mut replies = client
        .subscribe(inbox.clone())
        .await
        .map_err(|error| AuthError::Broker(error.to_string()))?;
    client
        .publish_with_reply("$SYS.REQ.SERVER.PING", inbox, Vec::new().into())
        .await
        .map_err(|error| AuthError::Broker(error.to_string()))?;
    let mut answered = false;
    while let Ok(Some(reply)) =
        tokio::time::timeout(Duration::from_millis(500), replies.next()).await
    {
        // A no-responders status has an empty body and identifies nobody.
        let Ok(reply) = serde_json::from_slice::<serde_json::Value>(&reply.payload) else {
            continue;
        };
        match reply
            .pointer("/server/id")
            .and_then(serde_json::Value::as_str)
        {
            Some(id) if id == server => return Ok(false),
            Some(_) => answered = true,
            None => {}
        }
    }
    Ok(answered)
}

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
    .map_err(|_| AuthError::Unavailable)?;
    let inventory = match inventory {
        // A NATS server takes a new ID each start, so a server that has gone
        // took its sockets with it. Silence alone proves nothing, since
        // credentials outside the system account hear none either.
        Err(error) if error.kind() == async_nats::RequestErrorKind::NoResponders => {
            return if server_is_gone(client, server).await? {
                Ok(())
            } else {
                Err(AuthError::Unavailable)
            };
        }
        inventory => inventory.map_err(|error| AuthError::Broker(error.to_string()))?,
    };
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

    /// Publish the issuer chain links that the registry may still lack.
    ///
    /// A rotation's key signs from the moment its transaction commits, but a
    /// participant pinned to an earlier key can authenticate it only through
    /// the chain link, which the outbox publishes later. Anything Auth signed
    /// therefore leaves Auth only after this returns: until the link is
    /// published, a participant must discard what the new key signed.
    pub(crate) async fn publish_issuer_chain(
        self: &Arc<Self>,
        material: &kv::Store,
    ) -> Result<(), AuthError> {
        use std::sync::atomic::Ordering;
        let store = Arc::clone(self);
        let head = blocking(move || {
            store.read(|connection| {
                Ok(connection.query_row(
                    "SELECT coalesce(max(sequence),0) FROM auth_issuer_rotations",
                    [],
                    |row| row.get::<_, u64>(0),
                )?)
            })
        })
        .await?;
        let published = self.published_rotation.load(Ordering::Acquire);
        for sequence in published.saturating_add(1)..=head {
            self.publish_verification_material(material, "rotation", &sequence.to_string())
                .await?;
        }
        self.published_rotation.fetch_max(head, Ordering::AcqRel);
        Ok(())
    }

    /// One ordinary worker turn: resume one enforcement page and a bounded
    /// outbox batch. Publication acknowledges durable JetStream acceptance;
    /// physical closure completes only after exact system-account evidence.
    /// Advance one bounded enforcement page and deliver committed broker effects.
    /// Count progress in both phases so drains cannot stop at effect-free pages.
    /// `notices` is a connection in the account sessions and deployments'
    /// Operation storage live in.
    pub(crate) async fn process_effects(
        self: &Arc<Self>,
        material: &kv::Store,
        revocations: &kv::Store,
        system: &Client,
        notices: &Client,
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
        // Every effect was committed under the current issuer or an earlier
        // one, and what it publishes must be verifiable on arrival.
        if !effects.is_empty() {
            if let Err(error) = self.publish_issuer_chain(material).await {
                let (store, claimed) = (Arc::clone(self), effects);
                blocking(move || {
                    for effect in &claimed {
                        store.retry_effect(effect, now)?;
                    }
                    Ok::<_, AuthError>(())
                })
                .await?;
                return Err(error);
            }
        }
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
                            .ok_or_else(|| {
                                AuthError::NotFound(format!("no revocation for session {session}"))
                            })?;
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
                    "issuer_notice" => {
                        notices
                            .publish(trellis_protocol::ISSUER_NOTICE_SUBJECT, Default::default())
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
                        notices
                            .flush()
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
                        Ok(())
                    }
                    "issuer_activate" => {
                        let sequence = effect.payload["sequence"]
                            .as_u64()
                            .ok_or(AuthError::Denied)?;
                        let store = Arc::clone(self);
                        blocking(move || store.activate_issuer(sequence, now)).await
                    }
                    "providers_notice" => {
                        let api = effect.payload["apiId"].as_str().ok_or(AuthError::Denied)?;
                        let subject = trellis_protocol::derive_providers_notice_subject(api)
                            .map_err(|_| AuthError::Denied)?;
                        notices
                            .publish(subject, Default::default())
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
                        notices
                            .flush()
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
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
                    "operation_storage_delete" => {
                        let deployment = effect.payload["deploymentId"]
                            .as_str()
                            .ok_or(AuthError::Denied)?;
                        super::framework_storage::delete_operations(notices, deployment).await
                    }
                    _ => Err(AuthError::Invalid("unknown committed effect".into())),
                }
            }
            .await;
            // Broker waits consume this batch's claim lifetime, too.
            let settled_at =
                now.saturating_add(i64::try_from(started.elapsed().as_secs()).unwrap_or(i64::MAX));
            let (store, claimed) = (Arc::clone(self), effect.clone());
            let settled = match result {
                Ok(()) => blocking(move || store.complete_effect(&claimed, settled_at))
                    .await
                    .map(|done| completed += usize::from(done)),
                Err(error) if error.storage_failure() == StorageFailure::Permanent => Err(error),
                Err(error) => {
                    tracing::warn!(
                        action_id=%effect.action_id,
                        kind=%effect.kind,
                        attempt=effect.attempts,
                        error=%error,
                        "Auth effect remains pending"
                    );
                    failure = Some(error);
                    blocking(move || store.retry_effect(&claimed, settled_at))
                        .await
                        .map(|_| ())
                }
            };
            let Err(error) = settled else {
                continue;
            };
            match error.storage_failure() {
                StorageFailure::Unusable => return Err(error),
                // Its claim lapses, and the effect runs again.
                StorageFailure::Transient => failure = Some(error),
                StorageFailure::Permanent => {
                    tracing::error!(
                        action_id=%effect.action_id,
                        kind=%effect.kind,
                        attempt=effect.attempts,
                        error=%error,
                        "Auth effect set aside: trying it again cannot succeed"
                    );
                    let store = Arc::clone(self);
                    let reason = error.to_string();
                    if let Err(error) =
                        blocking(move || store.set_aside_effect(&effect, &reason, settled_at)).await
                    {
                        if error.storage_failure() == StorageFailure::Unusable {
                            return Err(error);
                        }
                        // Its claim lapses, and the effect runs, and is set
                        // aside, again.
                        failure = Some(error);
                    }
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }
}
