//! Authenticated first-context retrieval for an already admitted runtime.

use futures_util::StreamExt as _;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{future::Future, sync::Arc, time::Duration};
use trellis_protocol::{
    sign_admission_context, verify_admission_get, AdmissionContext, SignedAdmissionContext,
    SignedAdmissionGet, ADMISSION_GET_DOMAIN_V1,
};

use super::{
    authorization_sessions::{session_credential, AdmissionIdentity},
    AuthError, SqliteAuthorizationStore,
};

/// Operational-account responder; ordinary clients receive only their exact
/// ephemeral-key-derived publish subject from Callout's complete ACL.
pub(crate) struct OwnContextService {
    subscriber: async_nats::Subscriber,
    metadata: async_nats::Subscriber,
    client: async_nats::Client,
    material: async_nats::jetstream::kv::Store,
    store: Arc<SqliteAuthorizationStore>,
}

impl OwnContextService {
    pub(crate) async fn start(
        client: async_nats::Client,
        material: async_nats::jetstream::kv::Store,
        store: Arc<SqliteAuthorizationStore>,
    ) -> Result<Self, AuthError> {
        let subscriber = client
            .queue_subscribe(
                "trellis.auth.admission.*.get",
                "trellis.auth.admission".into(),
            )
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let metadata = client
            .queue_subscribe(
                trellis_protocol::ISSUER_METADATA_SUBJECT,
                "trellis.auth.metadata".into(),
            )
            .await
            .map_err(|_| AuthError::Unavailable)?;
        client.flush().await.map_err(|_| AuthError::Unavailable)?;
        Ok(Self {
            subscriber,
            metadata,
            client,
            material,
            store,
        })
    }

    pub(crate) async fn run(
        mut self,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<(), AuthError> {
        tokio::pin!(shutdown);
        let mut requests = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _=&mut shutdown=>break,
                completed=requests.join_next(), if !requests.is_empty()=> {
                    match completed {
                        Some(Err(_)) => return Err(AuthError::Unavailable),
                        Some(Ok(Err(_))) => {
                            tracing::warn!("own-context response publication unavailable")
                        }
                        _ => {}
                    }
                }
                message=self.subscriber.next()=> {
                    let Some(message) = message else {
                        return Err(AuthError::Unavailable);
                    };
                    let Some(reply) = message.reply else {
                        continue;
                    };
                    // Never use Auth's operational publish rights as a deputy for
                    // an arbitrary reply destination, including error/overload paths.
                    if message.payload.len() > 4096 {
                        continue;
                    }
                    let Ok(request) =
                        serde_json::from_slice::<SignedAdmissionGet>(&message.payload)
                    else {
                        continue;
                    };
                    let Ok(inbox) = trellis_protocol::admission_inbox_prefix(
                        &request.request.session_public_key,
                    ) else {
                        continue;
                    };
                    if trellis_protocol::admission_get_subject(&request.request.session_public_key)
                        .ok()
                        .as_deref()
                        != Some(message.subject.as_str())
                        || !reply.starts_with(&format!("{inbox}."))
                        || reply.len() > 512
                        || reply.split('.').any(|part| {
                            part.is_empty()
                                || part.contains(['*', '>'])
                                || part.chars().any(char::is_whitespace)
                        })
                    {
                        continue;
                    }
                    if requests.len() >= 32 {
                        continue;
                    }
                    let store = self.store.clone();
                    let client = self.client.clone();
                    let material = self.material.clone();
                    requests.spawn(async move {
                        let destination = reply.clone();
                        let publication_store = store.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            store
                                .own_context_with_digest(
                                    &message.payload,
                                    message.subject.as_str(),
                                    destination.as_str(),
                                    crate::clock::unix_now(),
                                )
                                .and_then(|(response, digest)| {
                                    let authority = trellis_protocol::parse_session_authority(
                                        response.response.session_authority.as_bytes(),
                                        trellis_protocol::MAX_SESSION_AUTHORITY_BYTES,
                                    )?;
                                    let dependencies = authority
                                        .unsigned
                                        .apis
                                        .iter()
                                        .map(|api| ("catalog", api.catalog_snapshot_digest.clone()))
                                        .chain(authority.unsigned.provider_bindings.iter().map(
                                            |binding| {
                                                (
                                                    "provider",
                                                    binding.provider_certificate_digest.clone(),
                                                )
                                            },
                                        ))
                                        .collect::<Vec<_>>();
                                    Ok((serde_json::to_vec(&response)?, digest, dependencies))
                                })
                        })
                        .await
                        .map_err(|_| AuthError::Unavailable)?;
                        // The bootstrap envelope has no signed negative variant.
                        // Failed verification and overload remain transport failures,
                        // never unsigned credential-revocation instructions.
                        let Ok((bytes, digest, dependencies)) = result else {
                            return Ok(());
                        };
                        // A successful own-context reply makes application I/O immediately usable.
                        // Publish its exact immutable dependency closure before exposing that
                        // reply; the durable
                        // outbox remains the crash-recovery owner and may idempotently repeat this
                        // put.
                        for (kind, key) in dependencies {
                            publication_store
                                .publish_verification_material(&material, kind, &key)
                                .await?;
                        }
                        publication_store
                            .publish_verification_material(&material, "authority", &digest)
                            .await?;
                        client
                            .publish(reply, bytes.into())
                            .await
                            .map_err(|_| AuthError::Unavailable)
                    });
                }
                message=self.metadata.next()=> {
                    let Some(message) = message else {
                        return Err(AuthError::Unavailable);
                    };
                    let Some(reply) = message.reply else { continue };
                    // NATS admission ACLs restrict this subject to admitted users;
                    // never reflect their unsigned lookup into API/control subjects.
                    if message.payload.is_empty()
                        || message.payload.len() > 4096
                        || reply.len() > 512
                        || !reply.starts_with("_INBOX.")
                        || reply.split('.').count() < 3
                        || reply.split('.').any(|part| {
                            part.is_empty()
                                || part.contains(['*', '>'])
                                || part.chars().any(char::is_whitespace)
                        })
                        || requests.len() >= 32
                    {
                        continue;
                    }
                    let store = self.store.clone();
                    let client = self.client.clone();
                    requests.spawn(async move {
                        let destination = reply.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            store.issuer_metadata(
                                &message.payload,
                                message.subject.as_str(),
                                destination.as_str(),
                                crate::clock::unix_now(),
                            )
                        })
                        .await
                        .map_err(|_| AuthError::Unavailable)?;
                        let Ok(bytes) = result else { return Ok(()) };
                        client
                            .publish(reply, bytes.into())
                            .await
                            .map_err(|_| AuthError::Unavailable)
                    });
                }
            }
        }
        self.subscriber
            .unsubscribe()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        self.metadata
            .unsubscribe()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(result) = requests.join_next().await {
                result.map_err(|_| AuthError::Unavailable)??;
            }
            Ok::<(), AuthError>(())
        })
        .await;
        match drained {
            Ok(result) => result,
            Err(_) => {
                requests.abort_all();
                Err(AuthError::Unavailable)
            }
        }
    }
}

impl SqliteAuthorizationStore {
    /// Resolve only protocol-selected public verification material, from the
    /// same snapshot as its fresh issuer signature. No caller policy is granted.
    pub(super) fn issuer_metadata(
        &self,
        bytes: &[u8],
        subject: &str,
        reply: &str,
        now: i64,
    ) -> Result<Vec<u8>, AuthError> {
        use trellis_protocol::{
            sign_issuer_metadata, IssuerMetadata, IssuerMetadataQuery, IssuerMetadataRequest,
            IssuerMetadataResult,
        };
        if bytes.is_empty()
            || bytes.len() > 4096
            || subject != trellis_protocol::ISSUER_METADATA_SUBJECT
            || reply.len() > 512
            || !reply.starts_with("_INBOX.")
            || reply.split('.').count() < 3
            || reply.split('.').any(|part| {
                part.is_empty()
                    || part.contains(['*', '>'])
                    || part.chars().any(char::is_whitespace)
            })
        {
            return Err(AuthError::Denied);
        }
        let request: IssuerMetadataRequest = serde_json::from_slice(bytes)?;
        if request.trellis_instance_id != self.settings.instance_id
            || request.audience_nats_account != self.settings.account
        {
            return Err(AuthError::Denied);
        }
        self.transaction(|connection| {
            self.current_signer(connection)?;
            let mut response = IssuerMetadata {
                format: trellis_protocol::ISSUER_METADATA_DOMAIN_V1.into(),
                issuer_key_id: self.current_issuer(connection)?.key_id,
                request,
                request_subject: subject.into(),
                reply_subject: reply.into(),
                result: IssuerMetadataResult::Unavailable,
                issued_at: now,
                expires_at: now.saturating_add(30),
            };
            // Validate canonical nonce, selectors, scope and routing before SQL.
            response.digest()?;
            let result = match &response.request.query {
                IssuerMetadataQuery::IssuerKey { key_id } => {
                    Self::resolve_issuer_in(connection, key_id).and_then(|issuer| {
                        if issuer.state == trellis_protocol::AuthorityIssuerState::Active {
                            return Err(AuthError::Denied);
                        }
                        Ok(serde_json::to_string(&issuer)?)
                    })
                }
                IssuerMetadataQuery::AuthorityResolve { context_digest } => {
                    super::rpc_credentials::read_authority(connection, context_digest)
                        .and_then(|value| Ok(serde_json::to_string(&value)?))
                }
                IssuerMetadataQuery::AuthorityStatus {
                    authorization_session_id,
                } => {
                    let session = authorization_session_id
                        .parse()
                        .map_err(|_| AuthError::Denied)?;
                    Self::authorization_session_status_in(connection, session)
                        .and_then(|value| Ok(serde_json::to_string(&value)?))
                }
                IssuerMetadataQuery::IssuerChain {
                    pinned_key_id,
                    target_key_id,
                } => super::rpc_credentials::read_chain(
                    connection,
                    &trellis_runtime_apis::types::AuthIssuerChainRequest {
                        pinned_key_id: pinned_key_id.clone().into(),
                        target_key_id: target_key_id.clone().into(),
                        extra: Default::default(),
                    },
                )
                .and_then(|value| Ok(serde_json::to_string(&value)?)),
            };
            response.result = match result {
                Ok(payload) => IssuerMetadataResult::Value { payload },
                Err(AuthError::NotFound) => IssuerMetadataResult::Missing,
                Err(_) => IssuerMetadataResult::Unavailable,
            };
            match sign_issuer_metadata(response.clone(), &self.current_signer(connection)?) {
                Ok(signed) => Ok(serde_json::to_vec(&signed)?),
                Err(_) => {
                    response.result = IssuerMetadataResult::Unavailable;
                    Ok(serde_json::to_vec(&sign_issuer_metadata(
                        response,
                        &self.current_signer(connection)?,
                    )?)?)
                }
            }
        })
    }

    pub(super) fn attachment_registry(
        &self,
        connection: &rusqlite::Connection,
        authority: &trellis_protocol::UnsignedSessionAuthority,
        desired: &trellis_protocol::TransportAuthorizationV1,
        admitted: &trellis_protocol::TransportAuthorizationV1,
        attachment: &str,
    ) -> Result<Value, AuthError> {
        let mut registry = self.admission_registry(connection)?;
        registry["transportAuthorization"] = serde_json::to_value(desired)?;
        registry["admittedTransportAuthorization"] = serde_json::to_value(admitted)?;
        registry["attachmentId"] = Value::String(attachment.into());
        if let Some(bootstrap) = self.native_bootstrap(connection, authority)? {
            registry["authorization"] = bootstrap;
        }
        Ok(registry)
    }

    /// Project the installed participant revision and its committed resources;
    /// never substitute a generated caller descriptor or an empty binding for
    /// a resource that the installed native participant requires.
    pub(super) fn native_bootstrap(
        &self,
        connection: &rusqlite::Connection,
        authority: &trellis_protocol::UnsignedSessionAuthority,
    ) -> Result<Option<Value>, AuthError> {
        let (deployment, instance, participant) = match &authority.binding {
            trellis_protocol::SessionBinding::Service {
                deployment_id,
                instance_id,
                participant_id,
            }
            | trellis_protocol::SessionBinding::Device {
                deployment_id,
                instance_id,
                participant_id,
            } => (deployment_id, instance_id, participant_id),
            _ => return Ok(None),
        };
        let projection =
            super::native_projection::project_instance(connection, instance, authority.issued_at)?;
        if projection.deployment_id != *deployment
            || projection.participant_id != *participant
            || projection.principal_id != authority.principal_id
        {
            return Err(AuthError::Denied);
        }
        let digest = projection.participant_digest;
        let resources = projection.resources;
        if Some(super::sqlite::digest(&resources)?) != authority.resource_bindings_digest {
            return Err(AuthError::Denied);
        }
        let graph = match projection.graph {
            Some(graph) => graph,
            None => trellis_idl::compile_evidence(serde_json::from_str(&projection.evidence)?)
                .map_err(|error| AuthError::Invalid(error.to_string()))?,
        };
        let installed = graph
            .packages()
            .values()
            .flat_map(|package| package.participants().values())
            .find(|value| value.identity().as_str() == participant)
            .ok_or(AuthError::Denied)?;
        let runtime: trellis_rs::service::ServiceResourceBindings =
            serde_json::from_value(resources).map_err(|error| {
                AuthError::Invalid(format!(
                    "invalid committed native resource bindings: {error}"
                ))
            })?;
        for (name, definition) in installed.resources() {
            let name = name.as_str();
            let present = match definition {
                trellis_idl::ResourceDefinition::State { .. } => runtime.state.contains_key(name),
                trellis_idl::ResourceDefinition::Kv { .. } => runtime.kv.contains_key(name),
                trellis_idl::ResourceDefinition::Store { .. } => runtime.store.contains_key(name),
                trellis_idl::ResourceDefinition::Job { .. } => runtime
                    .jobs
                    .as_ref()
                    .is_some_and(|jobs| jobs.queues.contains_key(name)),
                trellis_idl::ResourceDefinition::Consumer { .. } => {
                    runtime.event_consumers.contains_key(name)
                }
            };
            if !present && !definition.optional() {
                return Err(AuthError::Unavailable);
            }
        }
        let companion = if let Some(definition) = installed.companion() {
            let client = format!("trellis.device.{deployment}.companion");
            let grant: Option<String> = connection
                .query_row(
                    "SELECT c.oauth_grant_id \
                    FROM auth_device_companions c \
                    JOIN auth_oauth_grants g USING(oauth_grant_id) \
                    JOIN auth_login_sessions l USING(login_session_id) \
                    JOIN auth_principals p ON p.principal_id=g.principal_id \
                    WHERE c.instance_id=?1 \
                    AND c.state='active' \
                    AND c.companion_request_id=?2 \
                    AND g.client_id=?3 AND g.binding_kind='native' \
                    AND g.revoked_at IS NULL \
                    AND g.expires_at>?4 \
                    AND l.revoked_at IS NULL \
                    AND l.expires_at>?4 \
                    AND p.state='active' \
                    AND EXISTS(SELECT 1 \
                    FROM auth_oauth_families f \
                    WHERE f.oauth_grant_id=g.oauth_grant_id \
                    AND f.revoked_at IS NULL \
                    AND f.expires_at>?4)",
                    params![
                        instance,
                        definition.request.as_str(),
                        client,
                        authority.issued_at
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            grant.map(|grant| {
                json!({
                    "requestId":definition.request.as_str(),
                    "oauthGrantId":grant,
                    "clientId":client,
                    "required":!definition.optional
                })
            })
        } else {
            None
        };
        Ok(Some(json!({
            "participantId":participant,
            "participantDigest":digest,
            "resourceRuntime":runtime,
            "companion":companion
        })))
    }

    /// The signed bootstrap identifies bounded immutable-material resolution;
    /// it never grows with the catalog or the installation's issuer history.
    pub(super) fn admission_registry(
        &self,
        connection: &rusqlite::Connection,
    ) -> Result<Value, AuthError> {
        let sequence: u64 = connection.query_row(
            "SELECT coalesce(max(sequence),0) FROM auth_issuer_rotations",
            [],
            |row| row.get(0),
        )?;
        Ok(json!({
            "materialBucket": super::broker::MATERIAL_BUCKET,
            "revocationBucket": super::broker::REVOCATION_BUCKET,
            "issuer": self.current_issuer(connection)?,
            "rotationSequence": sequence.to_string(),
        }))
    }

    /// Verify possession and actual NATS routing before reading any context.
    /// Recheck live policy inside the same snapshot used to sign the reply.
    #[cfg(test)]
    pub(crate) fn own_context(
        &self,
        bytes: &[u8],
        subject: &str,
        reply: &str,
        now: i64,
    ) -> Result<SignedAdmissionContext, AuthError> {
        self.own_context_with_digest(bytes, subject, reply, now)
            .map(|(response, _)| response)
    }

    fn own_context_with_digest(
        &self,
        bytes: &[u8],
        subject: &str,
        reply: &str,
        now: i64,
    ) -> Result<(SignedAdmissionContext, String), AuthError> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(AuthError::Denied);
        }
        let request: SignedAdmissionGet = serde_json::from_slice(bytes)?;
        verify_admission_get(&request, subject, reply, &self.settings.instance_id, now)?;
        self.transaction(|connection| {
            self.current_signer(connection)?;
            let count: i64 = connection.query_row(
                "SELECT count(DISTINCT s.authorization_session_id) \
                FROM auth_authorization_sessions s \
                JOIN auth_attachments a USING(authorization_session_id) \
                WHERE s.runtime_id=?1 \
                AND s.session_public_key=?2 \
                AND s.state='active' \
                AND a.state IN ('pending','admitted')",
                params![
                    request.request.runtime_id,
                    request.request.session_public_key
                ],
                |row| row.get(0),
            )?;
            // A key reused across different bindings is ambiguous, not permission
            // to choose whichever principal has the most authority.
            if count != 1 {
                return Err(AuthError::Denied);
            }
            let record: Option<(String, String, String)> = connection
                .query_row(
                    "SELECT s.authorization_session_id,a.attachment_id,\
                    a.transport_policy_json \
                    FROM auth_authorization_sessions s \
                    JOIN auth_attachments a USING(authorization_session_id) \
                    WHERE s.runtime_id=?1 \
                    AND s.session_public_key=?2 \
                    AND a.attachment_id=?3 \
                    AND s.state='active' \
                    AND a.state IN ('pending','admitted')",
                    params![
                        request.request.runtime_id,
                        request.request.session_public_key,
                        request.request.attachment_id
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let (session, attachment, admitted) = record.ok_or(AuthError::Denied)?;
            let identity = AdmissionIdentity {
                credential: session_credential(connection, &session)?,
                refresh_family_id: connection.query_row(
                    "SELECT original_refresh_family_id \
                    FROM auth_authorization_sessions \
                    WHERE authorization_session_id=?1",
                    [&session],
                    |row| row.get(0),
                )?,
                runtime_id: request.request.runtime_id.clone(),
                session_public_key: request.request.session_public_key.clone(),
                inbox_prefix: trellis_protocol::admission_inbox_prefix(
                    &request.request.session_public_key,
                )?,
            };
            if !reply.starts_with(&format!("{}.", identity.inbox_prefix)) {
                return Err(AuthError::Denied);
            }
            let issued = match self.issue_authority_in_transaction(
                connection,
                &identity,
                Some(&session),
                now,
            )? {
                Ok(issued) => issued,
                Err(error) => return Ok(Err(error)),
            };
            let desired = self.compile_transport(connection, &issued.authority.unsigned)?;
            let admitted: trellis_protocol::TransportAuthorizationV1 =
                serde_json::from_str(&admitted)?;
            admitted.validate()?;
            let registry = self.attachment_registry(
                connection,
                &issued.authority.unsigned,
                &desired,
                &admitted,
                &attachment,
            )?;
            let response = sign_admission_context(
                AdmissionContext {
                    format: ADMISSION_GET_DOMAIN_V1.into(),
                    issuer_key_id: self.current_issuer(connection)?.key_id,
                    trellis_instance_id: self.settings.instance_id.clone(),
                    audience_nats_account: self.settings.account.clone(),
                    runtime_id: request.request.runtime_id,
                    request_nonce: request.request.request_nonce,
                    request_subject: subject.into(),
                    reply_subject: reply.into(),
                    session_authority: super::sqlite::json(&issued.authority)?,
                    registry,
                    issued_at: now,
                    expires_at: issued
                        .authority
                        .unsigned
                        .expires_at
                        .min(now.saturating_add(30)),
                },
                &self.current_signer(connection)?,
            )?;
            Ok(Ok((response, issued.digest)))
        })?
    }
}
