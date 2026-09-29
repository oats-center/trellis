use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension};

use super::super::authority::ContextRepository;
use super::super::authority::{
    IssuanceConnection, IssuanceCredential, IssuanceCredentialRecord, IssuanceSnapshot,
};
use super::super::context::{AuthorizationContextRecord, AuthorizationContextSelector};
use super::super::{AuthorizationStateError, GrantOwnerKind, ResourceBindingEvidence};
use super::common::sql_error;
use super::evidence::load_deployment;
use super::principals::load_principal;
use super::sessions::load_session;
use super::SqliteAuthorizationStore;

impl SqliteAuthorizationStore {
    pub(crate) async fn get_resource_bindings(
        &self,
        owner_kind: GrantOwnerKind,
        owner_id: String,
        participant_id: String,
        installed_revision: u64,
    ) -> Result<Vec<ResourceBindingEvidence>, AuthorizationStateError> {
        self.run_read(move |connection| {
            // Reading present materialization through one pinned revision needs
            // that revision's declaration to interpret the current physical
            // resource; the current approval fences which of those declarations
            // survive.
            let (_, participant) = super::grants::load_installed_participant(
                connection,
                &participant_id,
                Some(installed_revision),
            )?
            .ok_or(AuthorizationStateError::ParticipantMissing)?;
            let Some(binding) = super::grants::load_grant_binding(
                connection,
                owner_kind,
                &owner_id,
                &participant_id,
            )?
            else {
                return Ok(Vec::new());
            };
            load_resource_bindings(
                connection,
                owner_kind,
                &owner_id,
                &participant_id,
                &binding.approved_resources,
                binding.revision,
                &participant.projection,
            )
        })
        .await
    }

    /// Lists every durable context in a selector scope, revoked or not, so
    /// transport reevaluation sees attachments the same change just revoked.
    pub(crate) async fn list_contexts_by_selector(
        &self,
        selector: AuthorizationContextSelector,
    ) -> Result<Vec<AuthorizationContextRecord>, AuthorizationStateError> {
        self.run_read(move |connection| {
            super::super::context::list_sql_contexts_by_selector(connection, &selector)
        })
        .await
    }
}

#[async_trait]
impl ContextRepository for SqliteAuthorizationStore {
    async fn load_issuance_snapshot(
        &self,
        request: &IssuanceConnection,
    ) -> Result<IssuanceSnapshot, AuthorizationStateError> {
        let request = request.clone();
        self.run_read(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            let snapshot = sqlite_issuance_snapshot(&transaction, &request, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(snapshot)
        })
        .await
    }
}

pub(in crate::platform::auth) fn sqlite_issuance_snapshot(
    connection: &Connection,
    request: &IssuanceConnection,
    pinned_revision: Option<u64>,
) -> Result<IssuanceSnapshot, AuthorizationStateError> {
    let connection_id = request.connection_id.parse::<ulid::Ulid>().map_err(|_| {
        AuthorizationStateError::InvalidRecord("connectionId must be a ULID".to_owned())
    })?;
    if connection_id.to_string() != request.connection_id {
        return Err(AuthorizationStateError::InvalidRecord(
            "connectionId must be canonical".to_owned(),
        ));
    }
    super::super::domain::validate_ed25519_public_key("sessionKey", &request.session_public_key)?;
    let (credential, principal_id, owner_kind, owner_id, participant_id, participant_revision) =
        match &request.credential {
            IssuanceCredential::Login(id) => {
                let login =
                    load_session(connection, id)?.ok_or(AuthorizationStateError::SessionMissing)?;
                // A user login is immutable session identity: a caller-supplied pin
                // that disagrees with the login's own revision fails closed rather
                // than silently adopting another vocabulary.
                if pinned_revision.is_some_and(|revision| revision != login.installed_revision) {
                    return Err(AuthorizationStateError::InvalidRecord(
                        "login participant revision does not match its session".to_owned(),
                    ));
                }
                // An immutable user login selects the participant revision whose
                // consent created it; a later binding change moves present
                // authority, not the vocabulary this login runs.
                (
                    IssuanceCredentialRecord::Login(login.clone()),
                    login.principal_id.clone(),
                    super::super::GrantOwnerKind::User,
                    login.principal_id,
                    login.participant_id,
                    Some(login.installed_revision),
                )
            }
            IssuanceCredential::Native(id) => {
                let identity = super::provisioning::load_provisioned_identity(connection, id)?
                    .ok_or(AuthorizationStateError::IdentityMissing)?;
                let instance =
                    super::evidence::load_runtime_instance(connection, &identity.instance_id)?
                        .ok_or(AuthorizationStateError::InstanceInactive)?;
                let deployment = load_deployment(connection, &identity.deployment_id)?
                    .ok_or(AuthorizationStateError::DeploymentInactive)?;
                let device = super::evidence::load_device(
                    connection,
                    &identity.principal_id,
                    &identity.deployment_id,
                )?;
                let delegation = super::evidence::load_device_delegation(
                    connection,
                    &identity.principal_id,
                    &identity.deployment_id,
                )?;
                let delegation_session = delegation
                    .as_ref()
                    .and_then(|delegation| delegation.user_login_session_id.as_deref())
                    .map(|session_id| load_session(connection, session_id))
                    .transpose()?
                    .flatten();
                let delegation_binding = delegation_session
                    .as_ref()
                    .map(|session| {
                        super::grants::load_grant_binding(
                            connection,
                            GrantOwnerKind::User,
                            &session.principal_id,
                            &session.participant_id,
                        )
                    })
                    .transpose()?
                    .flatten();
                let delegation_principal = delegation_session
                    .as_ref()
                    .map(|session| load_principal(connection, &session.principal_id))
                    .transpose()?
                    .flatten();
                let principal_id = identity.principal_id.clone();
                let owner_id = deployment.deployment_id.clone();
                let participant_id = deployment.participant_id.clone();
                // A native instance is evaluated against the participant revision it
                // is actually running, not the deployment's current desired revision.
                // An explicit pinned revision comes from the durable context being
                // reevaluated, whose scope must not drift with later adoptions.
                let participant_revision = pinned_revision.unwrap_or(instance.installed_revision);
                (
                    IssuanceCredentialRecord::Native(Box::new(
                        super::super::authority::NativeIssuanceCredentialRecord {
                            identity: Box::new(identity),
                            instance,
                            deployment,
                            device,
                            delegation,
                            delegation_session,
                            delegation_principal,
                            delegation_binding,
                        },
                    )),
                    principal_id,
                    super::super::GrantOwnerKind::Deployment,
                    owner_id,
                    participant_id,
                    Some(participant_revision),
                )
            }
        };
    let principal = load_principal(connection, &principal_id)?
        .ok_or(AuthorizationStateError::PrincipalMissing)?;
    let binding =
        super::grants::load_grant_binding(connection, owner_kind, &owner_id, &participant_id)?
            .ok_or(AuthorizationStateError::NotAuthorized)?;
    let participant_revision = participant_revision.unwrap_or(binding.installed_revision);
    let (_, participant) = super::grants::load_installed_participant(
        connection,
        &participant_id,
        Some(participant_revision),
    )?
    .ok_or(AuthorizationStateError::ParticipantMissing)?;
    // Resource evidence is keyed by the participant revision whose vocabulary
    // interprets the single current physical resource: it proves that the
    // physical resource that exists now is usable as that revision expects. The
    // pin therefore selects how present materialization is interpreted; it never
    // selects an old physical resource. The current binding remains the
    // authority ceiling that decides which of those declarations survive.
    let resources = load_resource_bindings(
        connection,
        owner_kind,
        &owner_id,
        &participant_id,
        &binding.approved_resources,
        binding.revision,
        &participant.projection,
    )?;
    // Include the mutable selected API provider bindings in the snapshot so
    // the issuance concurrency token covers the exact policy inputs.
    let binding_scope = match owner_kind {
        GrantOwnerKind::User => participant_id.as_str(),
        GrantOwnerKind::Deployment => owner_id.as_str(),
    };
    let api_bindings = {
        let mut statement = connection
            .prepare(
                "SELECT api_id, provider_deployment_id FROM auth_api_bindings
                     WHERE participant_id = ?1 ORDER BY api_id",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map([binding_scope], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        rows.into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let issuer = connection.query_row(
        "SELECT key_id, public_key FROM auth_authorization_issuers WHERE is_current = 1 AND revoked_at IS NULL",
        [], |row| Ok(trellis_protocol::AuthorizationIssuerKey {
            key_id: row.get(0)?, public_key: row.get(1)?, state: trellis_protocol::AuthorizationIssuerState::Active,
        }),
    ).optional().map_err(sql_error)?.ok_or(AuthorizationStateError::IssuerMissing)?;
    issuer
        .verifying_key()
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok(IssuanceSnapshot {
        connection: request.clone(),
        credential,
        principal,
        binding,
        participant,
        participant_revision,
        resources,
        api_bindings,
        issuer,
    })
}

pub(in crate::platform::auth) fn load_eligible_authorization_issuer(
    connection: &Connection,
    key_id: &str,
    now_ms: i64,
) -> Result<trellis_protocol::AuthorizationIssuerKey, AuthorizationStateError> {
    let issuer = connection
        .query_row(
            "SELECT public_key, is_current, live_until_seconds, revoked_at FROM auth_authorization_issuers WHERE key_id = ?1",
            [key_id],
            |row| {
                let state = if row.get::<_, Option<i64>>(3)?.is_some() {
                    trellis_protocol::AuthorizationIssuerState::Revoked
                } else if row.get::<_, bool>(1)?
                    || row.get::<_, i64>(2)? >= now_ms.div_euclid(1_000)
                {
                    trellis_protocol::AuthorizationIssuerState::Active
                } else {
                    trellis_protocol::AuthorizationIssuerState::Retired
                };
                Ok(trellis_protocol::AuthorizationIssuerKey {
                    key_id: key_id.to_owned(),
                    public_key: row.get(0)?,
                    state,
                })
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or(AuthorizationStateError::IssuerMissing)?;
    issuer
        .verifying_key()
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
    Ok(issuer)
}

pub(in crate::platform::auth) fn load_resource_bindings(
    connection: &Connection,
    owner_kind: super::super::GrantOwnerKind,
    owner_id: &str,
    participant_id: &str,
    approved_resources: &[super::super::ApprovedResource],
    fence_revision: u64,
    participant: &super::super::evidence::ParticipantRuntimeProjection,
) -> Result<Vec<ResourceBindingEvidence>, AuthorizationStateError> {
    // For every resource declared by the connection's participant revision, the
    // one current `auth_resources` materialization is interpreted through that
    // revision's declaration vocabulary, fenced by the current grant binding
    // revision. Historical evidence rows are never a source: materialization
    // that is missing, removed, or reconciled at another binding revision yields
    // unavailable authority instead of decoding a stale historical physical
    // resource.
    let mut resources = Vec::new();
    for (local_name, declaration) in &participant.resources {
        let Some(approved) = approved_resources.iter().find(|approved| {
            approved.name == *local_name
                && super::resources::participant_kind(approved.kind) == declaration.kind
        }) else {
            continue;
        };
        // Only the declaration's hard commitment must stay compatible with the
        // present approval; desired capacity hints are not hard.
        if approved.commitment.history != declaration.history
            || approved.commitment.ttl_ms != declaration.ttl_ms
        {
            continue;
        }
        let Some(catalog) = super::resources::load_resource_by_identity(
            connection,
            owner_kind,
            owner_id,
            participant_id,
            declaration.kind,
            local_name,
        )?
        else {
            continue;
        };
        // Materialization reconciled at another binding revision is superseded
        // and must not be projected at all.
        if catalog.binding_revision != fence_revision {
            continue;
        }
        // A ready physical resource must still satisfy this revision's hard
        // retention contract; an incompatible present materialization cannot
        // carry the declared authority.
        if catalog.state == super::super::resources::ResourceCatalogState::Ready
            && !catalog.actual.as_ref().is_some_and(|actual| {
                super::resources::materialization_satisfies(declaration, actual)
            })
        {
            continue;
        }
        // Derive the runtime binding from the declaration plus the current
        // physical materialization, so declaration-dependent semantics (event
        // filters, job subjects) follow this revision's vocabulary. A row that
        // exists at the current binding revision but is not yet usable is
        // reported as unavailable, so issuance treats it as a retriable
        // materialization gap instead of usable historical state.
        let ready = catalog.state == super::super::resources::ResourceCatalogState::Ready
            && catalog.actual.is_some();
        let mut evidence = super::resources::resource_evidence(
            &catalog,
            participant,
            declaration,
            if ready { catalog.actual.as_ref() } else { None },
            if ready {
                None
            } else {
                catalog.readiness_reason.as_deref()
            },
            catalog.updated_at,
        )?;
        if !ready {
            evidence.state = super::super::ResourceBindingState::Unavailable;
        }
        resources.push(evidence);
    }
    Ok(resources)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::SigningKey;
    use sha2::{Digest, Sha256};
    use trellis_idl::project::{GenerateConfig, PackageManifest, PackageMetadata};
    use trellis_idl::{
        canonical_package, compile_project, CanonicalMode, PackageEvidence, PackageSourceEvidence,
        SourceUnit,
    };
    use trellis_protocol::{
        AuthorizationIssuerKey, AuthorizationIssuerState, GrantSet, ParticipantKind,
        ParticipantResourceKind, PermissionAction, PermissionTarget,
    };

    use rusqlite::params;

    use super::super::common::encode_json;
    use super::super::grants::{
        accept_package_evidence, install_participant, replace_grant_binding,
    };
    use super::super::sessions::insert_sql_session;
    use super::*;
    use crate::platform::auth::authority::{IssuanceConnection, IssuanceCredential};
    use crate::platform::auth::evidence::PackageEvidenceInput;
    use crate::platform::auth::resources::ResourceActual;
    use crate::platform::auth::{
        participant_resource_commitments, ApprovalMode, ApprovedResource, DelegationCeiling,
        GrantBindingReplacement, GrantBindingState, IssuableAuthorizationState,
        ParticipantBindingRecord, ResourceProviderIdentity, SessionRecord, SessionState,
        SqliteAuthorizationStore,
    };

    const PARTICIPANT_ID: &str = "projection-test.Operator";
    const OWNER_ID: &str = "user-1";
    const NOW: i64 = 1_000;

    // R1 and R2 share the `records` KV and the `work` job; only the job's update
    // declaration changes, so R2 declares an update subject prefix that R1 does
    // not. A pinned R1 connection must keep the R1 interpretation of the one
    // current physical job.
    const SOURCE_R1: &str = r#"
model Record { value: string; }

app Operator {
  kv records { title "Records"; description "Records."; schema Record; version 1; history 1; ttl 60s; }
  job work { title "Work"; description "Work."; payload Record; result Record; deadline 5s; }
  store optional files { title "Files"; description "Files."; ttl 0; desired_max_object 1MiB; desired_max_total 8MiB; }
}
"#;

    const SOURCE_R2: &str = r#"
model Record { value: string; }

app Operator {
  kv records { title "Records"; description "Records."; schema Record; version 1; history 1; ttl 60s; }
  job work { title "Work"; description "Work."; payload Record; result Record; update Record; deadline 5s; }
  store optional files { title "Files"; description "Files."; ttl 0; desired_max_object 1MiB; desired_max_total 8MiB; }
}
"#;

    fn compiled(source: &str) -> (ParticipantBindingRecord, String) {
        let manifest = PackageManifest {
            package: PackageMetadata {
                name: "projection-test".into(),
                version: "1.0.0".parse().expect("version"),
            },
            sources: BTreeMap::from([("contract".into(), "contract.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        };
        let graph = compile_project(
            &manifest,
            vec![SourceUnit {
                alias: "contract".into(),
                path: PathBuf::from("contract.trellis"),
                source: source.into(),
            }],
            BTreeMap::new(),
        )
        .expect("compile package");
        let evidence = PackageEvidence {
            root_package: "projection-test".into(),
            root_digest: graph.root_digest().into(),
            packages: vec![PackageSourceEvidence {
                name: "projection-test".into(),
                version: "1.0.0".parse().expect("version"),
                digest: graph.root_digest().into(),
                source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation)
                    .expect("canonical package"),
            }],
        };
        ParticipantBindingRecord::from_package_evidence(
            &PackageEvidenceInput {
                package_digest: evidence.root_digest.clone(),
                package_evidence: evidence,
                participant_path: "Operator".into(),
            },
            NOW,
        )
        .expect("participant binding")
    }

    fn replacement(
        approved: &[ApprovedResource],
        expected_revision: u64,
    ) -> GrantBindingReplacement {
        GrantBindingReplacement {
            owner_kind: GrantOwnerKind::User,
            owner_id: OWNER_ID.into(),
            participant_id: PARTICIPANT_ID.into(),
            installed_revision: 2,
            grants: GrantSet::new(Vec::new()),
            approval_mode: ApprovalMode::Capabilities,
            approved_capabilities: Vec::new(),
            approved_resources: approved.to_vec(),
            delegation_ceiling: DelegationCeiling {
                capabilities: Vec::new(),
                exact_restrictions: None,
                platform_privileges: Vec::new(),
            },
            approval_decision_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(b"projection-test")),
            companion_approved: false,
            platform_privileges: Vec::new(),
            expected_revision,
            expected_current_installed_revision: None,
            state: GrantBindingState::Active,
            expires_at: None,
            provenance: None,
        }
    }

    fn materialization(name: &str) -> ResourceActual {
        match name {
            "records" => ResourceActual::Kv {
                history: 1,
                ttl_ms: 60_000,
                max_value_bytes: Some(8_192),
            },
            "work" => ResourceActual::Job,
            "files" => ResourceActual::Store {
                ttl_ms: 0,
                max_object_bytes: Some(1_048_576),
                max_total_bytes: Some(8_388_608),
            },
            other => panic!("unknown fixture resource {other}"),
        }
    }

    async fn resolve(
        store: &SqliteAuthorizationStore,
        session_id: &str,
        session_public_key: &str,
    ) -> Result<IssuableAuthorizationState, AuthorizationStateError> {
        let snapshot = store
            .load_issuance_snapshot(&IssuanceConnection {
                credential: IssuanceCredential::Login(session_id.to_owned()),
                connection_id: ulid::Ulid::new().to_string(),
                session_public_key: session_public_key.to_owned(),
            })
            .await?;
        crate::platform::auth::issuance::resolve_snapshot(snapshot, NOW)
    }

    fn store_bucket(state: &IssuableAuthorizationState, local_name: &str) -> Option<String> {
        state
            .resource_bindings
            .iter()
            .find_map(|resource| match &resource.provider_identity {
                ResourceProviderIdentity::Store { bucket } if resource.local_name == local_name => {
                    Some(bucket.clone())
                }
                _ => None,
            })
    }

    fn materialize_present_resources(
        connection: &Connection,
        projection: &crate::platform::auth::evidence::ParticipantRuntimeProjection,
    ) -> Result<(), AuthorizationStateError> {
        for (local_name, declaration) in &projection.resources {
            let Some(catalog) = super::super::resources::load_resource_by_identity(
                connection,
                GrantOwnerKind::User,
                OWNER_ID,
                PARTICIPANT_ID,
                declaration.kind,
                local_name,
            )?
            else {
                continue;
            };
            let actual = materialization(local_name);
            connection
                .execute(
                    "UPDATE auth_resources SET state = 'ready', actual_json = ?1, updated_at = ?2 \
                     WHERE resource_id = ?3",
                    params![encode_json(&actual)?, NOW, catalog.resource_id],
                )
                .map_err(sql_error)?;
        }
        Ok(())
    }

    /// Overwrites the `records` KV materialization's retention window so a test
    /// can observe how the projection treats an incompatible physical resource.
    async fn set_records_retention(store: &SqliteAuthorizationStore, ttl_ms: u64) {
        let actual = ResourceActual::Kv {
            history: 1,
            ttl_ms,
            max_value_bytes: Some(8_192),
        };
        store
            .run({
                let owner = OWNER_ID.to_owned();
                let participant_id = PARTICIPANT_ID.to_owned();
                move |connection| {
                    connection
                        .execute(
                            "UPDATE auth_resources SET actual_json = ?1 \
                             WHERE owner_kind = 'user' AND owner_id = ?2 AND participant_id = ?3 \
                               AND kind = 'kv' AND local_name = 'records'",
                            params![encode_json(&actual)?, owner, participant_id],
                        )
                        .map_err(sql_error)?;
                    Ok(())
                }
            })
            .await
            .expect("set records retention");
    }

    fn job_updates_prefix(state: &IssuableAuthorizationState) -> Option<String> {
        state
            .resource_bindings
            .iter()
            .find_map(|resource| match &resource.provider_identity {
                ResourceProviderIdentity::JobQueue { updates_prefix, .. } => {
                    Some(updates_prefix.clone())
                }
                _ => None,
            })
            .expect("job resource binding")
    }

    fn has_resource(
        state: &IssuableAuthorizationState,
        kind: ParticipantResourceKind,
        resource_name: &str,
        action: PermissionAction,
    ) -> bool {
        state.grant_set.permissions().iter().any(|atom| {
            atom.action() == action
                && matches!(
                    atom.target(),
                    PermissionTarget::ParticipantResource { participant, resource, name }
                        if participant == PARTICIPANT_ID && *resource == kind && name == resource_name
                )
        })
    }

    /// Proves a pinned R1 connection derives its resource authority from the one
    /// current physical materialization using R1 declaration semantics, while a
    /// current R2 connection uses R2 semantics, and that removed or
    /// not-yet-reconciled materialization produces no authority.
    #[tokio::test]
    async fn pinned_resource_projection_uses_current_materialization_with_pinned_semantics() {
        let store = SqliteAuthorizationStore::open_in_memory().expect("store");
        let (r1, r1_json) = compiled(SOURCE_R1);
        let (r2, r2_json) = compiled(SOURCE_R2);
        assert_ne!(
            r1.participant_digest, r2.participant_digest,
            "R2 must be a distinct participant revision"
        );

        store
            .run({
                let r1 = r1.clone();
                let r2 = r2.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    accept_package_evidence(
                        &transaction,
                        &r1.package_digest,
                        "projection-test",
                        &r1_json,
                        false,
                        None,
                        NOW,
                    )?;
                    assert_eq!(install_participant(&transaction, &r1, None)?, 1);
                    accept_package_evidence(
                        &transaction,
                        &r2.package_digest,
                        "projection-test",
                        &r2_json,
                        false,
                        None,
                        NOW,
                    )?;
                    assert_eq!(install_participant(&transaction, &r2, None)?, 2);
                    transaction
                        .execute(
                            "INSERT INTO auth_principals (principal_id, kind, state, created_at, \
                             updated_at, version, disabled_at, revoked_at)
                             VALUES (?1, 'user', 'active', ?2, ?2, 1, NULL, NULL)",
                            params![OWNER_ID, NOW],
                        )
                        .map_err(sql_error)?;
                    transaction.commit().map_err(sql_error)
                }
            })
            .await
            .expect("install participant revisions and owner principal");

        let commitments = participant_resource_commitments(&r2).expect("commitments");
        // `files` is declared optional and stays unapproved until after R1 is
        // pinned, so a later approval proves the pinned view grows from current
        // materialization.
        let initial_commitments = commitments
            .iter()
            .filter(|approved| approved.name != "files")
            .cloned()
            .collect::<Vec<_>>();

        // First approval creates the pending catalog rows for the initial
        // commitments.
        let binding = store
            .run({
                let commitments = initial_commitments.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    let (binding, _) =
                        replace_grant_binding(&transaction, replacement(&commitments, 0), NOW)?;
                    transaction.commit().map_err(sql_error)?;
                    Ok(binding)
                }
            })
            .await
            .expect("initial approval");
        assert_eq!((binding.installed_revision, binding.revision), (2, 1));

        // Materialize the current physical resources as reconcile would.
        let projection = r2.projection.clone();
        store
            .run({
                let projection = projection.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    materialize_present_resources(&transaction, &projection)?;
                    transaction.commit().map_err(sql_error)
                }
            })
            .await
            .expect("materialize resources");

        // A second approval pass refreshes the stored grants against the now-ready
        // materialization, exactly as reconcile would.
        let binding = store
            .run({
                let commitments = initial_commitments.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    let (binding, _) =
                        replace_grant_binding(&transaction, replacement(&commitments, 1), NOW)?;
                    transaction.commit().map_err(sql_error)?;
                    Ok(binding)
                }
            })
            .await
            .expect("refresh approval");
        assert_eq!(binding.revision, 2);

        let issuer_key = SigningKey::from_bytes(&[9; 32]).verifying_key();
        store
            .activate_issuer(
                AuthorizationIssuerKey {
                    key_id: URL_SAFE_NO_PAD.encode(Sha256::digest(issuer_key.as_bytes())),
                    public_key: URL_SAFE_NO_PAD.encode(issuer_key.as_bytes()),
                    state: AuthorizationIssuerState::Active,
                },
                NOW,
            )
            .await
            .expect("activate issuer");

        let session = |seed: u8, installed_revision: u64| {
            let session_id = ulid::Ulid::new().to_string();
            let session_public_key = URL_SAFE_NO_PAD.encode(
                SigningKey::from_bytes(&[seed; 32])
                    .verifying_key()
                    .as_bytes(),
            );
            let session_key_id = URL_SAFE_NO_PAD.encode(Sha256::digest(session_id.as_bytes()));
            (
                SessionRecord {
                    session_id,
                    principal_id: OWNER_ID.into(),
                    participant_id: PARTICIPANT_ID.into(),
                    participant_kind: ParticipantKind::App,
                    installed_revision,
                    session_key_id,
                    session_public_key: session_public_key.clone(),
                    state: SessionState::Active,
                    created_at: NOW,
                    last_authenticated_at: NOW,
                    expires_at: None,
                    revoked_at: None,
                    version: 1,
                },
                session_public_key,
            )
        };
        let (r1_session, r1_key) = session(7, 1);
        let (r2_session, r2_key) = session(8, 2);
        store
            .run({
                let r1_session = r1_session.clone();
                let r2_session = r2_session.clone();
                move |connection| {
                    insert_sql_session(connection, &r1_session)?;
                    insert_sql_session(connection, &r2_session)?;
                    Ok(())
                }
            })
            .await
            .expect("issue login sessions");

        // A connection frozen at R1 keeps its own declaration interpretation of
        // the one present physical job, even though the current deployment runs R2.
        let r1_state = resolve(&store, &r1_session.session_id, &r1_key)
            .await
            .expect("R1 issuance resolves");
        assert_eq!(
            job_updates_prefix(&r1_state),
            None,
            "R1 must project the current job through its own declaration"
        );
        assert!(has_resource(
            &r1_state,
            ParticipantResourceKind::Kv,
            "records",
            PermissionAction::Read
        ));
        assert!(has_resource(
            &r1_state,
            ParticipantResourceKind::JobQueue,
            "work",
            PermissionAction::Submit
        ));

        // A new R2 connection uses the current declaration.
        let r2_state = resolve(&store, &r2_session.session_id, &r2_key)
            .await
            .expect("R2 issuance resolves");
        assert!(
            job_updates_prefix(&r2_state).is_some(),
            "R2 must project the current job through the current declaration"
        );

        // §11.3 step 3: the present materialization must still satisfy the
        // pinned declaration's hard retention contract. A ready physical row
        // with a shorter retention window than the declaration promises cannot
        // carry its authority, even though the approval still matches.
        set_records_retention(&store, 1_000).await;
        let denied = resolve(&store, &r1_session.session_id, &r1_key)
            .await
            .expect_err("an incompatible present materialization must deny issuance");
        assert!(
            matches!(
                denied,
                AuthorizationStateError::RequiredResourceUnavailable(_)
            ),
            "expected required-resource denial, got {denied:?}"
        );
        set_records_retention(&store, 60_000).await;

        // §11.2: present materialization is fenced by the current grant binding
        // revision, not the participant installed revision. Relabelling the same
        // physical row to the installed revision must project no evidence, while
        // old code comparing against `installed_revision` would have accepted it.
        let mut future_binding = binding.clone();
        future_binding.revision = binding.revision + 1;
        let projected = store
            .run({
                let binding = future_binding.clone();
                let projection = r1.projection.clone();
                move |connection| {
                    let resources = load_resource_bindings(
                        connection,
                        GrantOwnerKind::User,
                        OWNER_ID,
                        PARTICIPANT_ID,
                        &binding.approved_resources,
                        binding.revision,
                        &projection,
                    )?;
                    Ok(resources
                        .iter()
                        .any(|resource| resource.local_name == "records"))
                }
            })
            .await
            .expect("binding revision fence");
        assert!(
            !projected,
            "materialization reconciled at another binding revision must not be projected"
        );

        // Approve a compatible optional resource after R1 is pinned. Its current
        // materialization must make the appropriate pinned view available.
        let binding = store
            .run({
                let commitments = commitments.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    let (binding, _) =
                        replace_grant_binding(&transaction, replacement(&commitments, 2), NOW)?;
                    transaction.commit().map_err(sql_error)?;
                    Ok(binding)
                }
            })
            .await
            .expect("approve files");
        assert_eq!(binding.revision, 3);
        store
            .run({
                let projection = r2.projection.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    materialize_present_resources(&transaction, &projection)?;
                    transaction.commit().map_err(sql_error)
                }
            })
            .await
            .expect("materialize files");
        let binding = store
            .run({
                let commitments = commitments.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    let (binding, _) =
                        replace_grant_binding(&transaction, replacement(&commitments, 3), NOW)?;
                    transaction.commit().map_err(sql_error)?;
                    Ok(binding)
                }
            })
            .await
            .expect("refresh files approval");
        assert_eq!(binding.revision, 4);
        let grown = resolve(&store, &r1_session.session_id, &r1_key)
            .await
            .expect("R1 issuance after resource growth");
        assert!(
            store_bucket(&grown, "files").is_some(),
            "the pinned R1 view must grow from current materialization"
        );
        assert!(has_resource(
            &grown,
            ParticipantResourceKind::Store,
            "files",
            PermissionAction::Read
        ));

        // Removing the KV from current authority detaches it, so the pinned R1
        // connection loses the mandatory resource authority entirely.
        let without_records = commitments
            .iter()
            .filter(|approved| approved.name != "records")
            .cloned()
            .collect::<Vec<_>>();
        let binding = store
            .run({
                let approved = without_records.clone();
                move |connection| {
                    let transaction = connection.transaction().map_err(sql_error)?;
                    let (binding, _) =
                        replace_grant_binding(&transaction, replacement(&approved, 4), NOW)?;
                    transaction.commit().map_err(sql_error)?;
                    Ok(binding)
                }
            })
            .await
            .expect("remove kv approval");
        assert_eq!(binding.revision, 5);
        let removed = resolve(&store, &r1_session.session_id, &r1_key)
            .await
            .expect_err("a removed required resource must deny issuance");
        assert!(
            matches!(
                removed,
                AuthorizationStateError::RequiredResourceUnavailable(_)
            ),
            "expected required-resource denial, got {removed:?}"
        );

        // Removing the job too denies the pinned connection its remaining
        // mandatory resource authority.
        store
            .run(move |connection| {
                let transaction = connection.transaction().map_err(sql_error)?;
                replace_grant_binding(&transaction, replacement(&[], 5), NOW)?;
                transaction.commit().map_err(sql_error)
            })
            .await
            .expect("remove job approval");
        let removed = resolve(&store, &r1_session.session_id, &r1_key)
            .await
            .expect_err("a fully removed required resource set must deny issuance");
        assert!(
            matches!(
                removed,
                AuthorizationStateError::RequiredResourceUnavailable(_)
            ),
            "expected required-resource denial, got {removed:?}"
        );
    }
}
