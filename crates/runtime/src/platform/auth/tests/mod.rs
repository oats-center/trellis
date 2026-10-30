use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use trellis_idl::{
    canonical_package, compile_project,
    project::{GenerateConfig, PackageManifest, PackageMetadata},
    CanonicalMode, PackageEvidence, PackageSourceEvidence, SourceUnit,
};
use trellis_protocol::{
    verify_session_authority, ApiSurfaceKind, CatalogActionIdentity, PermissionAction,
    SessionAuthorityPurpose, SessionAuthorityVerificationInput, SessionAuthorityVerificationPolicy,
};

use super::sqlite::{digest, id};
use super::*;

mod administration;
mod catalog;
mod consent;
mod credentials;
mod enforcement;
mod groups;
mod initialization;
mod issuance;
mod issuer_metadata;
mod issuer_rotation;
mod native_identity;
mod native_oauth;
mod oauth_clients;
mod oauth_renewal;
mod oauth_transport;
mod oauth_upstream;
mod own_context;
mod policy;
mod policy_aggregates;
mod resources;
mod runtime_rotation;
mod sdk_rotation;
mod service_claims;
mod state;
mod telemetry;

pub(super) const NOW: i64 = 1_800_000_000;

fn account_key() -> &'static str {
    static ACCOUNT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ACCOUNT
        .get_or_init(|| nkeys::KeyPair::new_account().public_key())
        .as_str()
}
pub(super) const SOURCE: &str = r#"
model Document { id: string; }
api documents@v1 {
 title "Documents";
 description "Document management.";
 rpc Get { input Document; output Document; }
 rpc Put { input Document; output Document; }
 rpc Export { input Document; output Document; }
 capabilities {
  capability documents.read { title "Read"; description "Read documents."; grants "Sees contents."; allows { rpc Get; } }
  capability documents.write { title "Write"; description "Write documents."; grants "Changes contents."; allows { rpc Put; } }
 }
}
service Reader { use documents { required capability documents.read; capability documents.write; } }
"#;

pub(super) struct Fixture {
    pub(super) directory: tempfile::TempDir,
    pub(super) store: Arc<SqliteAuthorizationStore>,
    pub(super) admin: String,
    pub(super) principal: String,
    api: String,
    pub(super) read: String,
    pub(super) write: String,
    grant: String,
    pub(super) admission: AdmissionIdentity,
}

/// Admit the signed credential through the ordinary Auth Callout owner, then
/// decode its persisted verification material for component assertions.
pub(super) fn admit_credential(
    store: &SqliteAuthorizationStore,
    credential: &[u8],
    nkey: &str,
    now: i64,
) -> Result<super::authorization_sessions::IssuedAuthority, AuthError> {
    let (attachment, _) = store.admit_connection(credential, nkey, nkey, "broker", 1, now)?;
    Ok(super::authorization_sessions::IssuedAuthority {
        authority: resolve_authority(store, &attachment.context_digest)?,
        digest: attachment.context_digest,
    })
}

pub(super) fn fixture_admission(f: &Fixture, nkey: &str, now: i64) -> Vec<u8> {
    let admission = f
        .store
        .transaction(|connection| {
            f.store.sign_oauth_admission_in(
                connection,
                &f.admission,
                nkey,
                &super::policy::evaluate(connection, &f.admission.credential, now)?,
                now,
            )
        })
        .unwrap();
    serde_json::to_vec(&admission).unwrap()
}

pub(super) fn revoke_root(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    scope: &EnforcementScope,
    now: i64,
) -> Result<String, AuthError> {
    store.transaction(|connection| {
        store.revoke_root_in_transaction(connection, identity, scope, now)
    })
}

/// Exercise provisioned-proof verification and signing at the SQLite boundary;
/// these aggregate fixtures do not claim to materialize broker resources.
pub(super) fn issue_provisioned_authority(
    store: &SqliteAuthorizationStore,
    credential: &[u8],
    nkey: &str,
    now: i64,
) -> Result<super::authorization_sessions::IssuedAuthority, AuthError> {
    let (_, public) = nkeys::from_public_key(nkey).map_err(|_| AuthError::Denied)?;
    let inbox = trellis_protocol::admission_inbox_prefix(&URL_SAFE_NO_PAD.encode(public))?;
    store.transaction(|connection| {
        let identity =
            store.provisioned_admission_identity_in(connection, credential, nkey, &inbox, now)?;
        store.issue_authority_in_transaction(connection, &identity, None, now)
    })?
}

pub(super) fn resolve_authority(
    store: &SqliteAuthorizationStore,
    digest: &str,
) -> Result<trellis_protocol::SignedSessionAuthority, AuthError> {
    Ok(trellis_protocol::parse_session_authority(
        &store.resolve_verification_material("authority", digest)?,
        trellis_protocol::MAX_SESSION_AUTHORITY_BYTES,
    )?)
}

pub(super) fn mutation(actor: &str, input: &impl Serialize) -> Mutation {
    Mutation {
        actor: actor.into(),
        request_id: id(),
        request_digest: digest(input).unwrap(),
        session: None,
    }
}

/// Exercise the issuance owner inside the same committing boundary as admission.
pub(super) fn issue_authority(
    store: &SqliteAuthorizationStore,
    admission: &AdmissionIdentity,
    renew_session: Option<&str>,
    now: i64,
) -> Result<super::authorization_sessions::IssuedAuthority, AuthError> {
    // Retirement must commit even when issuance returns a typed denial.
    store.transaction(|connection| {
        store.issue_authority_in_transaction(connection, admission, renew_session, now)
    })?
}

pub(super) fn rotate_issuer(
    store: &SqliteAuthorizationStore,
    actor: &str,
    now: i64,
) -> Result<trellis_protocol::SignedIssuerRotation, AuthError> {
    let input = trellis_runtime_apis::types::AuthIssuerRotateRequest {
        expected_current_key_id: store.issuer()?.key_id.into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    store.rotate_issuer(&mutation(actor, &input), &input, now)
}

/// Create an ordinary passwordless user through the production account aggregate.
pub(super) fn create_user(store: &SqliteAuthorizationStore, actor: &str, now: i64) -> String {
    let input = trellis_runtime_apis::__types::trellis::AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        idempotency_key: id().into(),
        username: id().into(),
        display_name: "Test user".to_owned().into(),
        password: None,
        extra: Default::default(),
    };
    super::rpc_users::create_user(store, &mutation(actor, &input), &input, now)
        .unwrap()
        .user
        .principal_id
        .to_string()
}

/// Establish the same real SQLite refresh-family record used by the OAuth engine.
/// Complete a storage future synchronously. A plain thread has no Tokio
/// context, so Auth's blocking helper runs the work inline there.
pub(super) fn ready<F: std::future::Future + Send>(future: F) -> F::Output
where
    F::Output: Send,
{
    use futures_util::FutureExt;
    std::thread::scope(|scope| {
        scope
            .spawn(|| future.now_or_never().expect("inline storage future"))
            .join()
            .unwrap()
    })
}

pub(super) fn oauth_family(
    store: &SqliteAuthorizationStore,
    grant: &str,
    now: i64,
    expires: i64,
) -> String {
    use oauth_as::{store::Storage, token::RefreshTokenRecord, ScopeSet};
    let path = store.writer().unwrap().path().unwrap().to_owned();
    let authority = SqliteAuthorizationStore::from_connection(
        rusqlite::Connection::open(path).unwrap(),
        store.signer.clone(),
        store.settings.clone(),
        store.custody_directory.clone(),
    )
    .unwrap();
    let (client, jkt): (String, String) = authority
        .writer()
        .unwrap()
        .query_row(
            "SELECT client_id,durable_dpop_jkt FROM auth_oauth_grants WHERE oauth_grant_id=?1",
            [grant],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let storage = super::oauth::OAuthStorage::new(Arc::new(authority), &[17; 32]).unwrap();
    let family = id();
    let mut record = RefreshTokenRecord::new(
        id(),
        oauth_as::client::ClientId::new(client),
        Some(grant.into()),
        ScopeSet::empty(),
        family.clone(),
    );
    record.jkt = Some(jkt.into());
    record.grant_established_at =
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(now as u64);
    record.expires_at =
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(expires as u64));
    let token = record.refresh_token.clone();
    let outcome = ready(storage.put_refresh_token(record)).unwrap();
    assert_eq!(outcome, oauth_as::store::WriteOutcome::Applied);
    // Component authority fixtures need the established family, not a retained
    // refresh secret encrypted under a key belonging to another OAuth fixture.
    ready(storage.take_refresh_token(&token)).unwrap();
    family
}

fn evidence(source: &str) -> PackageEvidence {
    let graph = compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: "phase-two".into(),
                version: "1.0.0".parse().unwrap(),
            },
            sources: BTreeMap::from([("main".into(), "main.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        },
        vec![SourceUnit {
            alias: "main".into(),
            path: PathBuf::from("main.trellis"),
            source: source.into(),
        }],
        BTreeMap::new(),
    )
    .unwrap();
    PackageEvidence {
        root_package: graph.root().to_string(),
        root_digest: graph.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: graph.root().to_string(),
            version: graph.root_package().version().clone(),
            digest: graph.root_digest().into(),
            source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap(),
        }],
    }
}

pub(super) fn open(directory: &tempfile::TempDir, key: SigningKey) -> SqliteAuthorizationStore {
    let mut connection = rusqlite::Connection::open(directory.path().join("platform.db")).unwrap();
    // Production runs the platform database in WAL mode, with a separate reader.
    connection
        .pragma_update(None, "journal_mode", "wal")
        .unwrap();
    let migration = refinery::Migration::unapplied(
        "V1000__platform_init",
        include_str!("../../../storage/sqlite/platform/V1000__platform_init.sql"),
    )
    .unwrap();
    refinery::Runner::new(&[migration])
        .run(&mut connection)
        .unwrap();
    SqliteAuthorizationStore::from_connection(
        connection,
        key,
        AuthSettings {
            avatar_external_fetch_enabled: true,
            password_min_length: 12,
            administration: Default::default(),
            upstream_tls_roots_pem: Vec::new(),
            instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            account: account_key().into(),
            oauth_issuer: "https://auth.example".into(),
            console_origin: "https://trellis.example".into(),
            authority_lifetime_seconds: 60,
            clock_skew_seconds: 5,
            oauth_admission_lifetime_seconds: 300,
            login_lifetime_seconds: 86400,
            enforcement_page_size: 2,
        },
        directory.path().join("issuer.keys"),
    )
    .unwrap()
}

pub(super) fn accept(
    store: &SqliteAuthorizationStore,
    admin: &str,
    source: &str,
    force: bool,
    now: i64,
) -> trellis_protocol::SignedCatalogSnapshot {
    let proposed = evidence(source);
    let graph = trellis_idl::compile_evidence(proposed.clone()).unwrap();
    let api = graph
        .root_package()
        .apis()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let review = store.review_api(admin, proposed, &api, now).unwrap();
    let acceptance = ApiAcceptance {
        review_id: review.review_id,
        reviewed_digest: review.definition_digest,
        force,
        acknowledge_breakage: force,
        confirm_consent_meanings: force,
        materially_changed: review.consent_changes,
        policy_edits: vec![],
    };
    store
        .accept_api(&mutation(admin, &acceptance), &acceptance, now)
        .unwrap()
}

pub(super) fn policy(
    store: &SqliteAuthorizationStore,
    admin: &str,
    command: PolicyMutation,
    now: i64,
) -> u64 {
    apply_policy(store, &mutation(admin, &command), &command, now).unwrap()
}

/// Grant `principal` administrative capabilities directly, as `admin`.
pub(super) fn administer(
    store: &SqliteAuthorizationStore,
    admin: &str,
    principal: &str,
    administration: &[Administration],
    now: i64,
) {
    for administration in administration {
        policy(
            store,
            admin,
            PolicyMutation::CapabilityGrant {
                principal_id: principal.into(),
                capability_id: administration.capability_id().into(),
                expected_revision: 0,
                expires_at: None,
            },
            now,
        );
    }
}

/// Commit a policy operation through the same aggregate mutator as RPC adapters.
pub(super) fn apply_policy(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    command: &PolicyMutation,
    now: i64,
) -> Result<u64, AuthError> {
    store.mutate(identity, "policy", command, now, |connection| {
        super::policy::apply_policy_in_transaction(connection, identity, command, now)
    })
}

fn approve(
    store: &SqliteAuthorizationStore,
    principal: &str,
    read: &str,
    write: &str,
    now: i64,
) -> String {
    let login = store
        .transaction(|connection| {
            SqliteAuthorizationStore::record_verified_login_in(
                connection,
                &VerifiedLogin {
                    principal_id: principal.into(),
                    provider_id: None,
                    upstream_issuer: None,
                    upstream_subject: None,
                    verified_claims: None,
                    expires_at: now + 3600,
                },
                now,
            )
        })
        .unwrap();
    let approval = grant_request(principal, &login, read, write, now);
    store
        .approve_grant(&mutation(principal, &approval), &approval, now)
        .unwrap()
}

pub(super) fn grant_request(
    principal: &str,
    login: &str,
    read: &str,
    write: &str,
    now: i64,
) -> GrantApproval {
    // RFC 7638 public P-256 thumbprint; private material never enters Auth.
    let jwk = serde_json::json!({
        "kty":"EC",
        "crv":"P-256",
        "x":"f83OJ3D2xF4tH8Ew7k6N4dQx7mD2GxLyD6uZJq9m7tQ",
        "y":"x_FEzRu9m36HLN1qrnUbcS1v6sV_4rxGO6ZGfStUdSU"
    });
    let jkt = URL_SAFE_NO_PAD.encode(Sha256::digest(
        super::sqlite::json(&jwk).unwrap().as_bytes(),
    ));
    GrantApproval {
        presentation: None,
        principal_id: principal.into(),
        login_session_id: login.into(),
        client_id: "native-client".into(),
        binding_kind: "native".into(),
        binding_value: jkt.clone(),
        durable_dpop_jkt: jkt,
        public_jwk: jwk,
        selection: CapabilitySelection {
            required: vec![read.into()],
            optional: vec![write.into()],
        },
        approved: vec![read.into(), write.into()],
        reused_remembered: vec![],
        declined: vec![],
        administration_confirmed: false,
        remember: true,
        remember_until: None,
        expires_at: now + 3600,
    }
}

pub(super) fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[42; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator(
            "admin",
            Some("a strong bootstrap password"),
            None,
            None,
            NOW,
        )
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let client = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Native client".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        implied_capabilities: vec![],
        development: false,
        metadata: serde_json::json!({}),
        expected_revision: 0,
    };
    store
        .transaction(|connection| {
            store.register_client_in_transaction(
                connection,
                &mutation(&admin, &client),
                &client,
                NOW,
            )
        })
        .unwrap();
    let catalog = accept(&store, &admin, SOURCE, false, NOW);
    let api = catalog.api_id;
    let read = catalog
        .capabilities
        .iter()
        .find(|cap| cap.capability_id.ends_with(":documents.read"))
        .unwrap()
        .capability_id
        .clone();
    let write = catalog
        .capabilities
        .iter()
        .find(|cap| cap.capability_id.ends_with(":documents.write"))
        .unwrap()
        .capability_id
        .clone();
    let principal = create_user(&store, &admin, NOW);
    for capability in [&read, &write] {
        policy(
            &store,
            &admin,
            PolicyMutation::CapabilityGrant {
                principal_id: principal.clone(),
                capability_id: capability.clone(),
                expected_revision: 0,
                expires_at: None,
            },
            NOW,
        );
    }
    let grant = approve(&store, &principal, &read, &write, NOW);
    let session_public_key =
        URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes());
    let inbox_prefix = trellis_protocol::admission_inbox_prefix(&session_public_key).unwrap();
    let admission = AdmissionIdentity {
        refresh_family_id: Some(oauth_family(&store, &grant, NOW, NOW + 86400)),
        credential: Credential::OAuthGrant(grant.clone()),
        runtime_id: URL_SAFE_NO_PAD.encode([7; 16]),
        session_public_key,
        inbox_prefix,
    };
    Fixture {
        directory,
        store,
        admin,
        principal,
        api,
        read,
        write,
        grant,
        admission,
    }
}

fn drain(store: &SqliteAuthorizationStore, now: i64) {
    while store.enforce_next_page(now).unwrap().is_some() {}
}

/// The signed-metadata status of one logical session.
pub(super) fn session_status(
    store: &SqliteAuthorizationStore,
    session: &str,
) -> trellis_protocol::AuthorizationSessionStatus {
    store
        .read(|connection| {
            SqliteAuthorizationStore::authorization_session_status_in(connection, session)
        })
        .unwrap()
}
