use crate::platform::auth::sqlite::common::{encode_enum, sql_error};
use crate::platform::auth::{
    builtins, GrantOwnerKind, MutationActor, NewSession, SessionRecord, SqliteAuthorizationStore,
    UserProfileRecord,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::SigningKey;
use rusqlite::params;
use serde_json::Map;
use sha2::{Digest as _, Sha256};
use trellis_protocol::{
    canonicalize_json, sign_authorization_context, AuthorizationIssuerKey,
    AuthorizationIssuerState, AuthorizationPrincipalKind, ParticipantKind, PlatformPrivilege,
    UnsignedAuthorizationContext, AUTHORIZATION_CONTEXT_FORMAT_V1,
};

pub(super) const NOW: i64 = 1_800_000_000_000;

pub(super) fn digest(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}

pub(super) fn profile_for(principal_id: &str) -> UserProfileRecord {
    UserProfileRecord {
        principal_id: principal_id.to_owned(),
        display_name: Some("User".to_owned()),
        email: None,
        image_url: None,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    }
}

pub(crate) async fn install_login_mutation_actor(
    store: &SqliteAuthorizationStore,
    now: i64,
) -> Result<MutationActor, Box<dyn std::error::Error>> {
    let signing_key = SigningKey::from_bytes(&[42; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
    let issuer = AuthorizationIssuerKey {
        key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(signing_key.verifying_key().as_bytes())),
        public_key: public_key.clone(),
        state: AuthorizationIssuerState::Active,
    };
    store.activate_issuer(issuer.clone(), now).await?;
    let participant = builtins::cli_participant_binding(now)?;
    let participant_id = participant.participant_id.clone();
    let grants = participant.resolve()?.required_grants.clone();
    let installed_revision = store.put_participant_binding(participant).await?;
    let principal_id = ulid::Ulid::new().to_string();
    let session_id = ulid::Ulid::new().to_string();
    let connection_id = ulid::Ulid::new().to_string();
    let session = SessionRecord::from_new(NewSession {
        session_id: session_id.clone(),
        principal_id: principal_id.clone(),
        participant_id: participant_id.clone(),
        participant_kind: ParticipantKind::App,
        session_public_key: public_key.clone(),
        created_at: now,
        expires_at: None,
    })?;
    let now_seconds = now.div_euclid(1_000);
    let unsigned = UnsignedAuthorizationContext {
        format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
        issuer_key_id: issuer.key_id.clone(),
        connection_id: connection_id.clone(),
        session_key: public_key.clone(),
        principal_id: principal_id.clone(),
        principal_kind: AuthorizationPrincipalKind::User,
        participant_id: participant_id.clone(),
        owner_kind: GrantOwnerKind::User,
        owner_id: principal_id.clone(),
        grant_revision: 1,
        identity_key_id: None,
        login_session_id: Some(session_id.clone()),
        deployment_id: None,
        instance_id: None,
        inbox_prefix: format!("_INBOX.{connection_id}"),
        issued_at: now_seconds,
        not_before: now_seconds,
        expires_at: now_seconds + 3_600,
        grants: grants.clone(),
        platform_privileges: vec![PlatformPrivilege::Admin],
        extensions: Map::new(),
        critical: Vec::new(),
    };
    let signed = sign_authorization_context(unsigned.clone(), &signing_key)?;
    let context_digest = signed.digest()?;
    let signed_context_json = canonicalize_json(&serde_json::to_value(&signed)?)?;
    let actor = MutationActor {
        context_digest: context_digest.clone(),
        principal_id: principal_id.clone(),
        participant_id: participant_id.clone(),
        owner_kind: GrantOwnerKind::User,
        owner_id: principal_id.clone(),
        grant_revision: 1,
        login_session_id: Some(session_id.clone()),
        session_public_key: public_key.clone(),
    };
    let grants_json = serde_json::to_string(&grants)?;
    let privileges_json = serde_json::to_string(&vec![PlatformPrivilege::Admin])?;
    let ceiling_json = serde_json::to_string(&super::super::super::DelegationCeiling {
        capabilities: Vec::new(),
        exact_restrictions: Some(grants),
        platform_privileges: vec![PlatformPrivilege::Admin],
    })?;
    store
        .run(move |connection| {
            connection
                .execute(
                    "UPDATE auth_authorization_issuers SET live_until_seconds = ?1 WHERE key_id = ?2",
                    params![now_seconds + 3_600, issuer.key_id],
                )
                .map_err(sql_error)?;
            connection
                .execute(
                    "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version, disabled_at, revoked_at)
                     VALUES (?1, 'user', 'active', ?2, ?2, 1, NULL, NULL)",
                    params![principal_id, now],
                )
                .map_err(sql_error)?;
            connection
                .execute(
                    "INSERT INTO auth_grant_bindings (owner_kind, owner_id, participant_id, installed_revision,
                         grants_json, approval_mode, approved_capabilities_json, approved_resources_json,
                         delegation_ceiling_json, approval_decision_digest,
                         approval_expected_grant_revision, companion_approved,
                         platform_privileges_json, revision, state, expires_at, provenance_json, created_at, updated_at)
                     VALUES ('user', ?1, ?2, ?3, ?4, 'exact', '[]', '[]', ?5,
                         ?6, 0, 0, ?7, 1, 'active', NULL, NULL, ?8, ?8)",
                    params![
                        principal_id,
                        participant_id,
                        installed_revision,
                        grants_json,
                        ceiling_json,
                        "A".repeat(43),
                        privileges_json,
                        now,
                    ],
                )
                .map_err(sql_error)?;
            connection
                .execute(
                    "INSERT INTO auth_sessions (session_id, principal_id, participant_id, participant_kind, session_public_key, session_key_id, state, created_at, last_authenticated_at, expires_at, revoked_at, version)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?7, NULL, NULL, 1)",
                    params![session.session_id, session.principal_id, session.participant_id, encode_enum(session.participant_kind)?, session.session_public_key, session.session_key_id, now],
                )
                .map_err(sql_error)?;
            connection.execute(
                "INSERT INTO auth_authorization_contexts (
                    context_digest, connection_id, session_public_key, inbox_prefix, principal_id,
                    principal_kind, participant_id, owner_kind, owner_id, grant_revision,
                    installed_revision, identity_key_id, login_session_id, issuer_key_id,
                    signed_context_json, issuance_snapshot_token, issued_at, not_before, refresh_at,
                    expires_at, state, published_at, revoked_at, revocation_reason, version
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'user', ?6, 'user', ?5, 1, ?7, NULL, ?8, ?9, ?10, ?11, ?12, ?12, ?12, ?13, 'active', ?12, NULL, NULL, 1)",
                params![context_digest, connection_id, public_key, unsigned.inbox_prefix, principal_id, participant_id, installed_revision, session_id, issuer.key_id, signed_context_json, digest(99), now_seconds, now_seconds + 3_600],
            ).map_err(sql_error)?;
            Ok(())
        })
        .await?;
    Ok(actor)
}
