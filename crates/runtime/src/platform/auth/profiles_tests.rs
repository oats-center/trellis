use super::*;
use crate::platform::auth::{
    phase_two_tests::{mutation, open, NOW},
    rpc_users,
    sqlite::id,
    SqliteAuthorizationStore,
};
use ed25519_dalek::SigningKey;
use serde_json::json;
use trellis_runtime_apis::{
    AuthUserCreateRequest, AuthUserIdentityUnlinkRequest, AuthUserProfileUpdateRequest,
};

#[test]
fn profile_enrichment_never_borrows_verification_from_a_different_email() {
    let mut token = VerifiedProfile::from_claims(
        &json!({"name":"Signed token name","email":"token@example.com"}),
    );
    token.fill(VerifiedProfile::from_claims(&json!({"name":"UserInfo name","given_name":"Given","email":"different@example.com","email_verified":true})));
    assert_eq!(token.name.as_deref(), Some("Signed token name"));
    assert_eq!(token.given_name.as_deref(), Some("Given"));
    assert_eq!(token.email.as_deref(), Some("token@example.com"));
    assert_ne!(token.email_verified, Some(true));
    let mut absent = VerifiedProfile::from_claims(&json!({"name":"Signed token name"}));
    absent.fill(VerifiedProfile::from_claims(
        &json!({"email":"userinfo@example.com","email_verified":true}),
    ));
    assert_eq!(absent.email.as_deref(), Some("userinfo@example.com"));
    assert_eq!(absent.email_verified, Some(true));
}

#[test]
fn owner_profile_updates_are_replayable_revision_checked_and_durable_without_changing_another_user()
{
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[37; 32]);
    let store = open(&directory, key.clone());
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator(
            "admin",
            Some("profile administrator password"),
            None,
            None,
            NOW,
        )
        .unwrap()
        .principal_id;
    let create: AuthUserCreateRequest = serde_json::from_value(
        json!({"username":"alice","displayName":"Shared name","idempotencyKey":id()}),
    )
    .unwrap();
    let alice = rpc_users::create_user(&store, &mutation(&admin, &create), &create, NOW)
        .unwrap()
        .user;
    let create: AuthUserCreateRequest = serde_json::from_value(json!({"username":"bob","displayName":"Shared name","email":"same@example.com","idempotencyKey":id()})).unwrap();
    let bob = rpc_users::create_user(&store, &mutation(&admin, &create), &create, NOW)
        .unwrap()
        .user;
    let actor = alice.principal_id.to_string();
    let input: AuthUserProfileUpdateRequest = serde_json::from_value(json!({
        "displayName":"Chosen name","givenName":"Alice","familyName":"Owner","email":"same@example.com", "gravatarEnabled":false,
        "expectedRevision":alice.account.unwrap().profile_revision.0.0.to_string(),"idempotencyKey":id(),
        "userId":bob.principal_id,
    })).unwrap();
    let identity = mutation(&actor, &input);
    let updated = rpc_users::update_profile(&store, &identity, &input, NOW + 1)
        .unwrap()
        .user;
    assert_eq!(updated.principal_id.to_string(), actor);
    assert_eq!(updated.display_name, "Chosen name");
    assert_eq!(updated.email.as_deref(), Some("same@example.com"));
    assert!(!updated.email_verified);
    let account = updated.account.unwrap();
    assert!(!account.gravatar_enabled);
    let replay = rpc_users::update_profile(&store, &identity, &input, NOW + 2)
        .unwrap()
        .user;
    assert_eq!(
        replay.account.unwrap().profile_revision,
        account.profile_revision
    );
    assert!(matches!(
        rpc_users::update_profile(&store, &mutation(&actor, &input), &input, NOW + 3),
        Err(AuthError::Conflict)
    ));
    let other = store
        .transaction(|connection| {
            rpc_users::read_user(&store, connection, &bob.principal_id.to_string(), true)
        })
        .unwrap();
    assert_eq!(other.display_name, "Shared name");
    assert_ne!(other.principal_id.to_string(), actor);
    drop(store);
    let store = open(&directory, key);
    let retained = store
        .transaction(|connection| rpc_users::read_user(&store, connection, &actor, true))
        .unwrap();
    assert_eq!(retained.given_name.as_deref(), Some("Alice"));
    assert_eq!(retained.family_name.as_deref(), Some("Owner"));
    assert_eq!(retained.email.as_deref(), Some("same@example.com"));
    let invalid: AuthUserProfileUpdateRequest = serde_json::from_value(json!({"email":"replace@example.com","clearEmail":true,"expectedRevision":account.profile_revision.0.0.to_string(),"idempotencyKey":id()})).unwrap();
    assert!(matches!(
        rpc_users::update_profile(&store, &mutation(&actor, &invalid), &invalid, NOW + 4),
        Err(AuthError::Invalid(_))
    ));
    let clear: AuthUserProfileUpdateRequest = serde_json::from_value(json!({"clearEmail":true,"expectedRevision":account.profile_revision.0.0.to_string(),"idempotencyKey":id()})).unwrap();
    let cleared = rpc_users::update_profile(&store, &mutation(&actor, &clear), &clear, NOW + 5)
        .unwrap()
        .user;
    assert!(cleared.email.is_none());
    assert!(!cleared.email_verified);
}

#[test]
fn linked_profile_projection_preserves_missing_claims_and_manual_choices_and_unlink_removes_only_upstream_data(
) {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[38; 32]));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator(
            "admin",
            Some("profile administrator password"),
            None,
            None,
            NOW,
        )
        .unwrap()
        .principal_id;
    let create: AuthUserCreateRequest = serde_json::from_value(json!({"username":"linked","displayName":"Manual name","password":"linked profile password","idempotencyKey":id()})).unwrap();
    let user = rpc_users::create_user(&store, &mutation(&admin, &create), &create, NOW)
        .unwrap()
        .user;
    let actor = user.principal_id.to_string();
    let provider = id();
    let registration = crate::platform::auth::lifecycle::OidcProviderRegistration {
        provider_id: provider.clone(),
        issuer: "https://identity.example".into(),
        display_name: "Identity".into(),
        client_id: "trellis".into(),
        sealed_client_secret: None,
        config: json!({"scopes":["openid","profile","email"]}),
        claim_freshness_seconds: 3600,
        expected_revision: 0,
    };
    store
        .transaction(|connection| {
            store.register_oidc_provider_in_transaction(
                connection,
                &mutation(&admin, &registration),
                &registration,
                NOW,
            )?;
            SqliteAuthorizationStore::link_oidc_identity_in(
                connection, &actor, &provider, "subject", NOW,
            )?;
            Ok(())
        })
        .unwrap();
    let profile = VerifiedProfile::from_claims(
        &json!({"name":"Provider name","given_name":"Linked","family_name":"Person","email":"verified@example.com","email_verified":true,"picture":"https://images.example/person.png"}),
    );
    store
        .transaction(|connection| {
            ingest(connection, &actor, &provider, "subject", &profile, NOW + 1)
        })
        .unwrap();
    store
        .transaction(|connection| {
            ingest(
                connection,
                &actor,
                &provider,
                "subject",
                &VerifiedProfile::default(),
                NOW + 2,
            )
        })
        .unwrap();
    let user = store
        .transaction(|connection| rpc_users::read_user(&store, connection, &actor, true))
        .unwrap();
    assert_eq!(user.display_name, "Manual name");
    assert_eq!(user.given_name.as_deref(), Some("Linked"));
    assert_eq!(user.email.as_deref(), Some("verified@example.com"));
    assert!(user.email_verified);
    assert!(store
        .transaction(|connection| ingest(
            connection,
            &admin,
            &provider,
            "subject",
            &profile,
            NOW + 3
        ))
        .is_err());
    let patch: AuthUserProfileUpdateRequest = serde_json::from_value(json!({"email":"chosen@example.com","givenName":"Chosen","expectedRevision":user.account.unwrap().profile_revision.0.0.to_string(),"idempotencyKey":id()})).unwrap();
    rpc_users::update_profile(&store, &mutation(&actor, &patch), &patch, NOW + 4).unwrap();
    store
        .transaction(|connection| {
            ingest(connection, &actor, &provider, "subject", &profile, NOW + 5)
        })
        .unwrap();
    let current = store
        .transaction(|connection| rpc_users::read_user(&store, connection, &actor, true))
        .unwrap();
    assert_eq!(current.given_name.as_deref(), Some("Chosen"));
    assert_eq!(current.email.as_deref(), Some("chosen@example.com"));
    assert!(!current.email_verified);
    let clear: AuthUserProfileUpdateRequest = serde_json::from_value(json!({"clearEmail":true,"expectedRevision":current.account.unwrap().profile_revision.0.0.to_string(),"idempotencyKey":id()})).unwrap();
    assert!(
        rpc_users::update_profile(&store, &mutation(&actor, &clear), &clear, NOW + 6)
            .unwrap()
            .user
            .email_verified
    );
    let identities = store
        .transaction(|connection| {
            rpc_users::read_identities(
                connection,
                &actor,
                &serde_json::from_value(json!({})).unwrap(),
            )
        })
        .unwrap();
    let link = identities
        .items
        .iter()
        .find(|identity| identity.provider_id.to_string() == provider)
        .unwrap();
    let unlink = AuthUserIdentityUnlinkRequest {
        identity_id: link.identity_id,
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    rpc_users::unlink_identity(&store, &mutation(&actor, &unlink), &unlink, NOW + 7).unwrap();
    let current = store
        .transaction(|connection| rpc_users::read_user(&store, connection, &actor, true))
        .unwrap();
    assert_eq!(current.display_name, "Manual name");
    assert_eq!(current.given_name.as_deref(), Some("Chosen"));
    assert!(current.family_name.is_none());
    assert!(current.email.is_none());
    assert!(store
        .transaction(|connection| read(connection, &actor))
        .unwrap()
        .picture
        .is_none());
}
