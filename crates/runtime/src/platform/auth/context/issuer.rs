use std::{cmp, sync::Arc};

use serde_json::{json, Map, Value};
use trellis_protocol::{
    authorization_context_refresh_at, canonicalize_json, sign_authorization_context,
    UnsignedAuthorizationContext, AUTHORIZATION_CONTEXT_FORMAT_V1,
};
use trellis_rs::client::{AuthorizationProviderCache, RuntimeAuthorizationTrust};

use super::{
    trust::VerifiedTrustMaterial, AuthorizationContextBundle, AuthorizationContextCommit,
    AuthorizationContextRecord, AuthorizationContextRegistry, AuthorizationContextRepository,
    AuthorizationContextState, AuthorizationRegistryBinding,
};
use crate::{
    config::AuthorizationConfig,
    platform::auth::{
        authority::{
            issuance_snapshot_token, ContextRepository, IssuanceConnection, IssuanceCredential,
            IssuanceSnapshotToken,
        },
        compile_transport_permissions,
        sqlite::{common::sql_error, contexts::sqlite_issuance_snapshot},
        AuthorizationStateError, IdempotencyResultRecord, IdempotentOutcome,
        IssuableAuthorizationState, SqliteAuthorizationStore, TransportPermissions,
    },
};

const SNAPSHOT_ATTEMPTS: usize = 3;

#[derive(Clone, Debug)]
pub(crate) struct AuthorizationContextIssueRequest {
    pub(crate) connection: IssuanceConnection,
    pub(crate) request_id: String,
    pub(crate) request_digest: String,
}

#[derive(Clone)]
pub(crate) struct AuthorizationContextService {
    repository: Arc<SqliteAuthorizationStore>,
    trust: Arc<VerifiedTrustMaterial>,
    registry: AuthorizationContextRegistry,
    validator_cache: AuthorizationProviderCache,
    config: AuthorizationConfig,
}

impl AuthorizationContextService {
    /// Resolve retained public issuer material without exposing signing secrets.
    pub(crate) async fn issuer_key(
        &self,
        key_id: String,
        now_ms: i64,
    ) -> Result<Option<trellis_protocol::AuthorizationIssuerKey>, AuthorizationStateError> {
        self.repository.get_issuer_key(key_id, now_ms).await
    }
    /// Clone of the validator cache backing local request/event verification.
    pub(crate) fn validator_cache(&self) -> AuthorizationProviderCache {
        self.validator_cache.clone()
    }

    pub(crate) async fn transport_permissions(
        &self,
        context: &trellis_protocol::VerifiedAuthorizationContext,
        now_seconds: i64,
    ) -> Result<TransportPermissions, AuthorizationStateError> {
        let signed = &context.signed_context().unsigned;
        let durable = self
            .repository
            .get_context_by_digest(context.context_digest())
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "authorization context is missing from durable state".to_owned(),
                )
            })?;
        let now_ms = seconds_to_millis(now_seconds)?;
        let current = self
            .repository
            .run_read(move |connection| {
                let transaction = connection.unchecked_transaction().map_err(sql_error)?;
                let active: bool = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM auth_authorization_contexts AS context
                     JOIN auth_authorization_issuers AS issuer ON issuer.key_id = context.issuer_key_id
                     WHERE context.context_digest = ?1 AND context.state = 'active'
                       AND context.revoked_at IS NULL AND context.published_at IS NOT NULL
                       AND context.not_before <= ?2 AND context.expires_at > ?2
                       AND issuer.revoked_at IS NULL)",
                        rusqlite::params![durable.context_digest, now_seconds],
                        |row| row.get(0),
                    )
                    .map_err(sql_error)?;
                if !active {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
                let scope = IssuanceConnection {
                    credential: match (&durable.login_session_id, &durable.identity_key_id) {
                        (Some(id), None) => IssuanceCredential::Login(id.clone()),
                        (None, Some(id)) => IssuanceCredential::Native(id.clone()),
                        _ => return Err(AuthorizationStateError::NotAuthorized),
                    },
                    connection_id: durable.connection_id.clone(),
                    session_public_key: durable.session_public_key.clone(),
                };
                let snapshot = sqlite_issuance_snapshot(&transaction, &scope)?;
                if issuance_snapshot_token(&snapshot)?.0 != durable.issuance_snapshot_token {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
                let current = crate::platform::auth::issuance::resolve_snapshot(snapshot, now_ms)?;
                if !current.matches_context(
                    &durable.signed_context()?.unsigned,
                    durable.installed_revision,
                ) {
                    return Err(AuthorizationStateError::NotAuthorized);
                }
                transaction.commit().map_err(sql_error)?;
                Ok(current)
            })
            .await?;
        let permissions = compile_transport_permissions(
            signed,
            &current.participant,
            &current.resource_bindings,
            &crate::platform::auth::current_api_bindings(
                self.repository.as_ref(),
                &current.participant,
                signed.deployment_id.as_deref(),
            )
            .await?,
            &AuthorizationRegistryBinding::from_config(&self.config),
        )?;
        Ok(permissions)
    }

    pub(crate) async fn run_janitor(
        self,
        stop: crate::shutdown::StopHandle,
    ) -> Result<(), crate::supervisor::RuntimeError> {
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?
                .as_secs();
            let now = i64::try_from(now).map_err(|_| {
                crate::supervisor::RuntimeError::Platform(
                    "context janitor time overflow".to_owned(),
                )
            })?;
            let expired = self
                .repository
                .expire_contexts(now)
                .await
                .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?;
            let deleted_idempotency = self
                .repository
                .delete_expired_context_idempotency(now)
                .await
                .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))?;
            if !expired.is_empty() || deleted_idempotency > 0 {
                tracing::debug!(
                    expired = expired.len(),
                    deleted_idempotency,
                    "authorization context janitor completed"
                );
            }
            tokio::select! {
                () = stop.stopped() => return Ok(()),
                () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            }
        }
    }

    pub(crate) async fn run_validator_cache(
        self,
        stop: crate::shutdown::StopHandle,
    ) -> Result<(), crate::supervisor::RuntimeError> {
        self.validator_cache
            .run_runtime({
                let (sender, receiver) = tokio::sync::watch::channel(());
                tokio::spawn(async move {
                    stop.stopped().await;
                    drop(sender);
                });
                receiver
            })
            .await
            .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))
    }

    pub(crate) async fn wait_for_validator_cache(
        &self,
    ) -> Result<(), crate::supervisor::RuntimeError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.validator_cache.wait_until_ready(),
        )
        .await
        .map_err(|_| {
            crate::supervisor::RuntimeError::Platform(
                "authorization validator cache did not become ready".to_owned(),
            )
        })?
        .map_err(|error| crate::supervisor::RuntimeError::Platform(error.to_string()))
    }

    pub(crate) async fn require_current_context(
        &self,
        connection_id: &str,
        context_digest: &str,
        now_seconds: i64,
    ) -> Result<AuthorizationContextRecord, AuthorizationStateError> {
        let context = self
            .repository
            .get_context_by_digest(context_digest)
            .await?
            .ok_or(AuthorizationStateError::AuthorityStale)?;
        let signed = context.signed_context()?;
        if context.connection_id != connection_id
            || context.state != AuthorizationContextState::Active
            || context.published_at.is_none()
            || signed.unsigned.not_before > now_seconds
            || context.expires_at <= now_seconds
        {
            return Err(AuthorizationStateError::AuthorityStale);
        }
        self.repository
            .get_issuer_key(
                context.issuer_key_id.clone(),
                seconds_to_millis(now_seconds)?,
            )
            .await?
            .filter(|issuer| issuer.state == trellis_protocol::AuthorizationIssuerState::Active)
            .ok_or(AuthorizationStateError::AuthorityStale)?;
        Ok(context)
    }

    pub(crate) async fn start(
        repository: Arc<SqliteAuthorizationStore>,
        nats: async_nats::Client,
        config: AuthorizationConfig,
        trellis_origin: String,
        now_seconds: i64,
    ) -> Result<Self, AuthorizationStateError> {
        let trust = Arc::new(
            VerifiedTrustMaterial::load(&config, now_seconds)
                .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
        );
        let provider_nats = nats.clone();
        let registry = AuthorizationContextRegistry::ensure(nats, &config).await?;
        repository
            .activate_issuer(trust.issuer.clone(), seconds_to_millis(now_seconds)?)
            .await?;
        let registry_binding = AuthorizationRegistryBinding::from_config(&config);
        let client_registry_binding =
            trellis_rs::client::AuthorizationRegistryBinding::from_runtime_parts(
                registry_binding.context_bucket.clone(),
            );
        let validator_cache = AuthorizationProviderCache::attach_runtime(
            provider_nats,
            &client_registry_binding,
            RuntimeAuthorizationTrust {
                trellis_origin,
                issuer: Some(trust.issuer.clone()),
                policy: trust.policy.clone(),
            },
        )
        .await
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        let service = Self {
            repository,
            trust,
            registry,
            validator_cache,
            config,
        };
        service.repair_unpublished(now_seconds).await?;
        let mut after = None;
        loop {
            let contexts = service
                .repository
                .list_revoked_contexts(after.as_deref(), 256)
                .await?;
            if contexts.is_empty() {
                break;
            }
            for context in &contexts {
                service.publish_revocation(context).await?;
            }
            after = contexts
                .last()
                .map(|context| context.context_digest.clone());
        }
        Ok(service)
    }

    pub(crate) async fn issue(
        &self,
        request: AuthorizationContextIssueRequest,
        now_seconds: i64,
    ) -> Result<AuthorizationContextBundle, AuthorizationStateError> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            match self.issue_once(&request, now_seconds).await {
                Err(AuthorizationStateError::StorageConflict) => continue,
                result => return result.map(|(bundle, _)| bundle),
            }
        }
        Err(AuthorizationStateError::ContextSnapshotChanged)
    }

    pub(crate) async fn issue_with_state(
        &self,
        request: AuthorizationContextIssueRequest,
        now_seconds: i64,
    ) -> Result<(AuthorizationContextBundle, IssuableAuthorizationState), AuthorizationStateError>
    {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            match self.issue_once(&request, now_seconds).await {
                Err(AuthorizationStateError::StorageConflict) => continue,
                result => return result,
            }
        }
        Err(AuthorizationStateError::ContextSnapshotChanged)
    }

    pub(crate) async fn repair_unpublished(
        &self,
        now_seconds: i64,
    ) -> Result<(), AuthorizationStateError> {
        self.repository.expire_contexts(now_seconds).await?;
        let mut after = None;
        loop {
            let contexts = self.repository.list_contexts(after.as_deref(), 256).await?;
            if contexts.is_empty() {
                break;
            }
            for context in &contexts {
                self.publish(context, now_seconds).await?;
            }
            after = contexts
                .last()
                .map(|context| context.context_digest.clone());
        }
        Ok(())
    }

    pub(crate) async fn publish_revocation(
        &self,
        context: &AuthorizationContextRecord,
    ) -> Result<(), AuthorizationStateError> {
        self.registry.publish_revocation(context).await
    }

    pub(crate) async fn dispatch_registry_action(
        &self,
        digest: &str,
        revocation: bool,
        now_seconds: i64,
    ) -> Result<(), AuthorizationStateError> {
        let context = self
            .repository
            .get_context_by_digest(digest)
            .await?
            .ok_or_else(|| {
                AuthorizationStateError::Storage("context action is missing".to_owned())
            })?;
        if revocation {
            self.registry.publish_revocation(&context).await?;
            self.validator_cache
                .apply_runtime_revocation(
                    digest,
                    context.revoked_at.ok_or_else(|| {
                        AuthorizationStateError::Storage(
                            "revoked context has no revocation time".to_owned(),
                        )
                    })?,
                )
                .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
        } else {
            self.publish(&context, now_seconds).await.map(|_| ())
        }
    }

    async fn issue_once(
        &self,
        request: &AuthorizationContextIssueRequest,
        now_seconds: i64,
    ) -> Result<(AuthorizationContextBundle, IssuableAuthorizationState), AuthorizationStateError>
    {
        let now_millis = seconds_to_millis(now_seconds)?;
        let snapshot = self
            .repository
            .load_issuance_snapshot(&request.connection)
            .await?;
        let initial = super::super::issuance::resolve_snapshot(snapshot, now_millis)?;
        crate::platform::auth::resolve_api_bindings(
            self.repository.as_ref(),
            &initial.participant,
            initial.deployment_id.as_deref(),
        )
        .await?;
        let snapshot = self
            .repository
            .load_issuance_snapshot(&request.connection)
            .await?;
        let snapshot_token = issuance_snapshot_token(&snapshot)?;
        if snapshot.issuer != self.trust.issuer {
            return Err(AuthorizationStateError::ContextSnapshotChanged);
        }
        let authorization = super::super::issuance::resolve_snapshot(snapshot.clone(), now_millis)?;
        let expires_at = context_expiry(&authorization, &self.config, now_seconds)?;

        if authorization.grant_set.permissions().len() > self.config.maximum_permissions {
            return Err(AuthorizationStateError::InvalidRecord(
                "authorization context exceeds configured bounds".to_owned(),
            ));
        }
        let unsigned = UnsignedAuthorizationContext {
            format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
            issuer_key_id: self.trust.issuer.key_id.clone(),
            connection_id: authorization.connection_id.clone(),
            session_key: authorization.session_public_key.clone(),
            principal_id: authorization.principal_id.clone(),
            principal_kind: authorization.principal_kind,
            participant_id: authorization.participant.participant_id.clone(),
            owner_kind: authorization.binding.owner_kind,
            owner_id: authorization.binding.owner_id.clone(),
            grant_revision: authorization.binding.revision,
            login_session_id: authorization.login_session_id.clone(),
            identity_key_id: authorization.identity_key_id.clone(),
            deployment_id: authorization.deployment_id.clone(),
            instance_id: authorization.instance_id.clone(),
            inbox_prefix: authorization.inbox_prefix.clone(),
            issued_at: now_seconds,
            not_before: cmp::max(
                now_seconds
                    - i64::try_from(self.config.allowed_clock_skew_seconds).map_err(|_| {
                        AuthorizationStateError::InvalidRecord("clock skew is too large".to_owned())
                    })?,
                0,
            ),
            expires_at,
            grants: authorization.grant_set.clone(),
            platform_privileges: authorization.binding.platform_privileges.clone(),
            extensions: Map::new(),
            critical: Vec::new(),
        };
        let signed = sign_authorization_context(unsigned, &self.trust.issuer_signing_key)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let signed_context_json = canonicalize_json(
            &serde_json::to_value(&signed)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        )
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        if signed_context_json.len() > self.config.maximum_context_bytes {
            return Err(AuthorizationStateError::InvalidRecord(
                "authorization context canonical JSON is too large".to_owned(),
            ));
        }
        let context_digest = signed
            .digest()
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let refresh_at = authorization_context_refresh_at(
            &context_digest,
            now_seconds,
            signed.unsigned.not_before,
            expires_at,
            u32::try_from(self.config.refresh_lead_seconds).map_err(|_| {
                AuthorizationStateError::InvalidRecord("refresh lead is too large".to_owned())
            })?,
            u32::try_from(self.config.refresh_jitter_seconds).map_err(|_| {
                AuthorizationStateError::InvalidRecord("refresh jitter is too large".to_owned())
            })?,
        )
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let context = AuthorizationContextRecord {
            context_digest: context_digest.clone(),
            principal_id: authorization.principal_id.clone(),
            principal_kind: authorization.principal_kind,
            owner_kind: authorization.binding.owner_kind,
            owner_id: authorization.binding.owner_id.clone(),
            participant_id: authorization.participant.participant_id.clone(),
            grant_revision: authorization.binding.revision,
            installed_revision: authorization.binding.installed_revision,
            identity_key_id: authorization.identity_key_id.clone(),
            login_session_id: authorization.login_session_id.clone(),
            connection_id: authorization.connection_id.clone(),
            session_public_key: authorization.session_public_key.clone(),
            inbox_prefix: authorization.inbox_prefix.clone(),
            issued_at: signed.unsigned.issued_at,
            not_before: signed.unsigned.not_before,
            issuer_key_id: self.trust.issuer.key_id.clone(),
            signed_context_json,
            issuance_snapshot_token: snapshot_token.0.clone(),
            refresh_at,
            expires_at,
            state: AuthorizationContextState::Active,
            published_at: None,
            revoked_at: None,
            revocation_reason: None,
            version: 1,
        };
        let commit = AuthorizationContextCommit {
            expected_snapshot_token: snapshot_token.clone(),
            context,
            idempotency: context_issue_idempotency(
                request,
                now_millis,
                expires_at,
                &context_digest,
            )?,
            now: now_seconds,
            minimum_remaining_seconds: i64::try_from(self.config.minimum_context_lifetime_seconds)
                .map_err(|_| AuthorizationStateError::ContextLifetimeUnavailable)?,
        };
        let context = match self.repository.commit_context(commit).await? {
            IdempotentOutcome::Applied(context) => context,
            IdempotentOutcome::Replayed(result) => {
                let context_digest = result
                    .get("contextDigest")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AuthorizationStateError::Storage("invalid context replay".to_owned())
                    })?;
                let context = self
                    .repository
                    .get_context_by_digest(context_digest)
                    .await?
                    .ok_or_else(|| {
                        AuthorizationStateError::Storage("context replay is missing".to_owned())
                    })?;
                require_reusable_context(
                    &context,
                    &snapshot_token,
                    &self.trust.issuer.key_id,
                    now_seconds,
                )?;
                context
            }
        };
        let context = self.publish(&context, now_seconds).await?;
        tracing::info!(
            context_digest = %context.context_digest,
            connection_id = %context.connection_id,
            owner_id = %context.owner_id,
            expires_at = context.expires_at,
            "issued authorization context"
        );
        Ok((self.bundle(&context, now_seconds).await?, authorization))
    }

    async fn publish(
        &self,
        context: &AuthorizationContextRecord,
        published_at: i64,
    ) -> Result<AuthorizationContextRecord, AuthorizationStateError> {
        self.registry.publish_context(context).await?;
        if context.published_at.is_some() {
            return Ok(context.clone());
        }
        self.repository
            .mark_context_published(&context.context_digest, published_at)
            .await
    }

    async fn bundle(
        &self,
        context: &AuthorizationContextRecord,
        now_seconds: i64,
    ) -> Result<AuthorizationContextBundle, AuthorizationStateError> {
        let issuer = self
            .repository
            .get_issuer_key(
                context.issuer_key_id.clone(),
                seconds_to_millis(now_seconds)?,
            )
            .await?
            .filter(|issuer| issuer.state == trellis_protocol::AuthorizationIssuerState::Active)
            .ok_or(AuthorizationStateError::AuthorityStale)?;
        Ok(AuthorizationContextBundle::from_runtime_parts(
            serde_json::from_str(&context.signed_context_json).map_err(|error| {
                AuthorizationStateError::InvalidRecord(format!(
                    "persisted authorization context is invalid: {error}"
                ))
            })?,
            issuer,
            trellis_rs::client::AuthorizationRegistryBinding::from_runtime_parts(
                self.config.context_bucket.clone(),
            ),
            trellis_rs::client::AuthorizationContextPolicy {
                allowed_clock_skew_seconds: self.trust.policy.allowed_clock_skew_seconds,
                maximum_context_lifetime_seconds: self
                    .trust
                    .policy
                    .maximum_context_lifetime_seconds,
                maximum_context_bytes: self.trust.policy.maximum_context_bytes,
                maximum_permissions: self.trust.policy.maximum_permissions,
                refresh_lead_seconds: self.config.refresh_lead_seconds.try_into().map_err(
                    |_| AuthorizationStateError::InvalidRecord("invalid refresh lead".to_owned()),
                )?,
                refresh_jitter_seconds: self.config.refresh_jitter_seconds.try_into().map_err(
                    |_| AuthorizationStateError::InvalidRecord("invalid refresh jitter".to_owned()),
                )?,
            },
        ))
    }
}

fn require_reusable_context(
    context: &AuthorizationContextRecord,
    snapshot_token: &IssuanceSnapshotToken,
    issuer_key_id: &str,
    now_seconds: i64,
) -> Result<(), AuthorizationStateError> {
    if context.state != AuthorizationContextState::Active
        || context.expires_at <= now_seconds
        || context.issuer_key_id != issuer_key_id
        || context.issuance_snapshot_token != snapshot_token.0
    {
        return Err(AuthorizationStateError::ContextSnapshotChanged);
    }
    Ok(())
}

fn context_expiry(
    authorization: &crate::platform::auth::IssuableAuthorizationState,
    config: &AuthorizationConfig,
    now_seconds: i64,
) -> Result<i64, AuthorizationStateError> {
    let signed_lifetime = config
        .context_lifetime_seconds
        .checked_sub(config.allowed_clock_skew_seconds)
        .ok_or(AuthorizationStateError::ContextLifetimeUnavailable)?;
    let mut expires_at = now_seconds
        .checked_add(i64::try_from(signed_lifetime).map_err(|_| {
            AuthorizationStateError::InvalidRecord("context lifetime is too large".to_owned())
        })?)
        .ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("context expiry overflow".to_owned())
        })?;
    if let Some(bound) = authorization.expires_at {
        expires_at = cmp::min(expires_at, bound.div_euclid(1_000));
    }
    let remaining = expires_at - now_seconds;
    if remaining
        < i64::try_from(config.minimum_context_lifetime_seconds)
            .map_err(|_| AuthorizationStateError::ContextLifetimeUnavailable)?
    {
        return Err(AuthorizationStateError::ContextLifetimeUnavailable);
    }
    Ok(expires_at)
}

fn seconds_to_millis(seconds: i64) -> Result<i64, AuthorizationStateError> {
    seconds
        .checked_mul(1_000)
        .ok_or_else(|| AuthorizationStateError::InvalidRecord("timestamp overflow".to_owned()))
}

fn context_issue_idempotency(
    request: &AuthorizationContextIssueRequest,
    now_millis: i64,
    expires_at: i64,
    context_digest: &str,
) -> Result<IdempotencyResultRecord, AuthorizationStateError> {
    Ok(IdempotencyResultRecord {
        scope_key: trellis_protocol::digest_json(&json!({
            "purpose": "authorizationContextIssue", "credential": request.connection.credential,
            "connectionId": request.connection.connection_id, "requestId": request.request_id,
        }))
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        purpose: "authorizationContextIssue".to_owned(),
        signer_id: match &request.connection.credential {
            IssuanceCredential::Login(id) | IssuanceCredential::Native(id) => id.clone(),
        },
        request_id: request.request_id.clone(),
        request_digest: request.request_digest.clone(),
        result: json!({ "contextDigest": context_digest }),
        created_at: now_millis,
        expires_at: seconds_to_millis(expires_at)?,
    })
}
