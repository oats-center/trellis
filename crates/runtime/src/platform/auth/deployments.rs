use super::Administration;
use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::VerifyingKey;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trellis_idl::{
    api_digest, compile_evidence, participant_digest, ActionKind, ActionSelection,
    InteractionDirection, PackageEvidence, ParticipantKind,
};

use super::authorization_sessions::ProviderImplementation;
use super::catalog::{action_identity, resolve_api};
use super::policy::capability;
use super::sqlite::{decode, digest, id, json, AuthError, Mutation, SqliteAuthorizationStore};

pub(super) const SELF_HEALTH_CAPABILITY: &str = "trellis.health.v1:health.report";
/// Every service instance may submit its own build as a participant update.
pub(super) const SERVICE_UPDATE_CAPABILITY: &str = "trellis.auth.v1:serviceUpdates.submit";
pub(super) const JOB_HISTORY_CAPABILITY: &str = "trellis.jobs.v1:history.read";
pub(super) const CONSUMER_RUNTIME_CAPABILITY: &str = "trellis.events.v1:replays.process";

/// The automatically assigned runtime right must never expand into domain actions.
pub(super) fn native_health_capability(
    connection: &crate::platform::auth::sqlite::Connection,
) -> Result<trellis_protocol::CapabilityAuthority, AuthError> {
    let cap = capability(connection, SELF_HEALTH_CAPABILITY)?;
    let mut statement = connection.prepare(
        "SELECT api_id,action_key \
        FROM auth_capability_actions \
        WHERE capability_id=?1 \
        AND identity_generation=?2",
    )?;
    let members = statement
        .query_map(
            params![SELF_HEALTH_CAPABILITY, cap.identity_generation.get()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let action = trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Event,
        name: "Heartbeat".into(),
        direction: trellis_protocol::PermissionAction::Publish,
    };
    if members != vec![("trellis.health@v1".into(), json(&action)?)] {
        return Err(AuthError::Denied);
    }
    Ok(cap)
}

/// Deployment authoring comes from native source evidence, never reconstructed
/// action schemas. The provisioner supplies resolved runtime resource commitments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DeploymentProvisioning {
    pub(crate) evidence: PackageEvidence,
    pub(crate) participant_id: String,
    pub(crate) expected_api_generations: BTreeMap<String, u64>,
    pub(crate) identity_public_key: String,
    pub(crate) resource_commitments: serde_json::Value,
    pub(crate) expires_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProvisionedDeployment {
    pub(crate) participant_id: String,
    pub(crate) principal_id: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: String,
    pub(crate) identity_key_id: String,
}

struct BuiltinProviderSource {
    evidence: PackageEvidence,
    participant_digest: String,
    public_key: String,
    key_id: String,
}

impl BuiltinProviderSource {
    fn from_seed<C: trellis_rs::generated::ParticipantDescriptor>(
        seed: &str,
    ) -> Result<Self, AuthError> {
        C::validate().map_err(|error| AuthError::Invalid(error.to_string()))?;
        let evidence: PackageEvidence =
            serde_json::from_value(serde_json::to_value(C::package_evidence())?)?;
        let graph = compile_evidence(evidence.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let definition = graph
            .root_package()
            .participants()
            .values()
            .find(|definition| definition.identity().as_str() == C::ID)
            .ok_or(AuthError::NotFound)?;
        if definition.kind() != ParticipantKind::Service
            || definition.companion().is_some()
            || definition
                .resources()
                .values()
                .any(|resource| !matches!(resource, trellis_idl::ResourceDefinition::Kv { .. }))
        {
            return Err(AuthError::Invalid(
                "built-in startup supports services with native KV declarations, \
                not companions or other resource kinds"
                    .into(),
            ));
        }
        let expected_digest = participant_digest(&graph, definition.identity())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(seed)
            .map_err(|_| AuthError::Denied)?;
        let raw: [u8; 32] = bytes.try_into().map_err(|_| AuthError::Denied)?;
        let public = URL_SAFE_NO_PAD.encode(
            ed25519_dalek::SigningKey::from_bytes(&raw)
                .verifying_key()
                .as_bytes(),
        );
        let key_id = URL_SAFE_NO_PAD.encode(Sha256::digest(
            URL_SAFE_NO_PAD
                .decode(&public)
                .map_err(|_| AuthError::Denied)?,
        ));
        Ok(Self {
            evidence,
            participant_digest: expected_digest,
            public_key: public,
            key_id,
        })
    }

    fn lookup<C: trellis_rs::generated::ParticipantDescriptor>(
        &self,
        connection: &crate::platform::auth::sqlite::Connection,
        now: i64,
    ) -> Result<ProvisionedDeployment, AuthError> {
        let row = connection
            .query_row(
                "SELECT p.principal_id,p.deployment_id,p.instance_id,p.participant_id,\
                p.state,d.state,i.installed_revision,b.installed_revision,\
                s.participant_digest \
                FROM auth_provisioned_identities p \
                JOIN auth_deployments d USING(deployment_id) \
                JOIN auth_instances i ON i.instance_id=p.instance_id \
                AND i.deployment_id=p.deployment_id \
                AND i.principal_id=p.principal_id \
                JOIN auth_deployment_bindings b ON b.deployment_id=p.deployment_id \
                JOIN auth_installed_participants s ON \
                s.participant_id=p.participant_id \
                AND s.revision=i.installed_revision \
                JOIN auth_principals a ON a.principal_id=p.principal_id \
                WHERE p.identity_key_id=?1 \
                AND p.identity_public_key=?2 \
                AND a.state='active' \
                AND i.state='active' \
                AND d.participant_id=p.participant_id \
                AND (d.expires_at IS NULL \
                OR d.expires_at>?3)",
                params![self.key_id, self.public_key, now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, u64>(6)?,
                        row.get::<_, u64>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            principal_id,
            deployment_id,
            instance_id,
            participant,
            state,
            deployment_state,
            revision,
            bound_revision,
            installed_digest,
        )) = row
        {
            if participant != C::ID
                || state != "active"
                || deployment_state != "active"
                || revision != bound_revision
            {
                return Err(AuthError::Denied);
            }
            if installed_digest != self.participant_digest {
                return Err(AuthError::Conflict);
            }
            return Ok(ProvisionedDeployment {
                participant_id: participant,
                principal_id,
                deployment_id,
                instance_id,
                identity_key_id: self.key_id.clone(),
            });
        }
        let known: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_provisioned_identities WHERE identity_key_id=?1)",
            [&self.key_id],
            |row| row.get(0),
        )?;
        Err(if known {
            AuthError::Denied
        } else {
            AuthError::NotFound
        })
    }
}

impl SqliteAuthorizationStore {
    /// Lease-owning host startup installs the bundled native provider through
    /// ordinary catalog acceptance and deployment approval. Existing identities
    /// are checked, never re-enabled or reassigned by startup.
    pub(crate) fn initialize_builtin_provider<C: trellis_rs::generated::ParticipantDescriptor>(
        &self,
        seed: &str,
        now: i64,
    ) -> Result<ProvisionedDeployment, AuthError> {
        let source = BuiltinProviderSource::from_seed::<C>(seed)?;
        let existing = self.transaction(|connection| {
            let admin: String = connection
                .query_row(
                    "SELECT principal_id FROM auth_bootstrap_administrator WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::Denied)?;
            Self::require_administration(connection, &admin, Administration::ManagePrincipals)?;
            match source.lookup::<C>(connection, now) {
                Ok(existing) => Ok(Some(existing)),
                Err(AuthError::NotFound) => Ok(None),
                Err(error) => Err(error),
            }
        })?;
        if let Some(existing) = existing {
            return Ok(existing);
        }
        let admin: String = self.transaction(|connection| {
            connection
                .query_row(
                    "SELECT principal_id FROM auth_bootstrap_administrator WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::Denied)
        })?;
        let mut expected_api_generations = BTreeMap::new();
        for api in C::IMPLEMENTED_API_IDS {
            match self.current_catalog(api) {
                Ok(catalog) => {
                    expected_api_generations.insert((*api).into(), catalog.generation.get());
                }
                Err(AuthError::NotFound) => {
                    let review = self.review_api(&admin, source.evidence.clone(), api, now)?;
                    let acceptance = super::catalog::ApiAcceptance {
                        review_id: review.review_id.clone(),
                        reviewed_digest: review.definition_digest,
                        force: false,
                        acknowledge_breakage: false,
                        confirm_consent_meanings: false,
                        materially_changed: vec![],
                        policy_edits: vec![],
                    };
                    let command = Mutation {
                        actor: admin.clone(),
                        request_id: review.review_id,
                        request_digest: digest(&acceptance)?,
                        session: None,
                    };
                    let catalog = self.accept_api(&command, &acceptance, now)?;
                    expected_api_generations.insert((*api).into(), catalog.generation.get());
                }
                Err(error) => return Err(error),
            }
        }
        let input = DeploymentProvisioning {
            evidence: source.evidence.clone(),
            participant_id: C::ID.into(),
            expected_api_generations,
            identity_public_key: source.public_key.clone(),
            resource_commitments: serde_json::json!({}),
            expires_at: None,
        };
        // These are pending commitments, not an empty admitted binding. The
        // declaration-derived materializer must commit actual resources before CONNECT.
        let request_hash =
            Sha256::digest(format!("builtin:{}:{}", self.settings.instance_id, C::ID));
        let mut request_bytes = [0; 16];
        request_bytes.copy_from_slice(&request_hash[..16]);
        let provisioned = self.provision_deployment(
            &Mutation {
                actor: admin,
                request_id: ulid::Ulid::from_bytes(request_bytes).to_string(),
                request_digest: digest(&input)?,
                session: None,
            },
            &input,
            now,
        )?;
        // Ordinary mutation replay returns its historical result. Startup may
        // only use it if the provisioned identity still exists and is active.
        self.transaction(|connection| {
            let current = source.lookup::<C>(connection, now)?;
            if current.principal_id != provisioned.principal_id
                || current.deployment_id != provisioned.deployment_id
                || current.instance_id != provisioned.instance_id
            {
                return Err(AuthError::Denied);
            }
            Ok(current)
        })
    }

    /// Trusted provisioning and Runtime startup use the same native participant
    /// installation and compiler as every other service/device identity.
    pub(crate) fn provision_deployment(
        &self,
        mutation: &Mutation,
        input: &DeploymentProvisioning,
        now: i64,
    ) -> Result<ProvisionedDeployment, AuthError> {
        self.mutate(mutation, "deployment.provision", input, now, |connection| {
            Self::require_administration(
                connection,
                &mutation.actor,
                Administration::ManagePrincipals,
            )?;
            let graph = compile_evidence(input.evidence.clone())
                .map_err(|error| AuthError::Invalid(error.to_string()))?;
            let title = graph
                .root_package()
                .participants()
                .values()
                .find(|participant| participant.identity().as_str() == input.participant_id)
                .ok_or(AuthError::NotFound)?
                .name();
            let deployment = Self::install_deployment_in_transaction(
                connection,
                &mutation.actor,
                input,
                None,
                title,
                now,
            )?;
            Self::provision_instance_in_transaction(
                connection,
                &deployment,
                Some(&input.identity_public_key),
                title,
                now,
            )
        })
    }

    /// Install native participant evidence and bind a deployment without creating
    /// a principal or allocating resources. Resource producers commit actual
    /// bindings separately before the credential can be admitted.
    pub(super) fn install_deployment_in_transaction(
        connection: &crate::platform::auth::sqlite::Connection,
        actor: &str,
        input: &DeploymentProvisioning,
        existing: Option<&str>,
        title: &str,
        now: i64,
    ) -> Result<String, AuthError> {
        if title.trim().is_empty() {
            return Err(AuthError::Invalid("deployment title is required".into()));
        }
        let graph = compile_evidence(input.evidence.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let participant = graph
            .root_package()
            .participants()
            .values()
            .find(|participant| participant.identity().as_str() == input.participant_id)
            .ok_or(AuthError::NotFound)?;
        let needs = graph
            .participant_needs(participant.identity())
            .ok_or(AuthError::NotFound)?;
        let participant_digest = participant_digest(&graph, participant.identity())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let required: Vec<String> = needs
            .required_capabilities()
            .iter()
            .map(ToString::to_string)
            .collect();
        let optional: Vec<String> = needs
            .optional_capabilities()
            .iter()
            .map(ToString::to_string)
            .collect();
        // Deploying a participant that selects administration changes who
        // holds it, which takes `trellis.auth.v1:platform.manage`.
        Self::require_platform_for(
            connection,
            actor,
            required.iter().chain(&optional).map(String::as_str),
        )?;
        if input.expires_at.is_some_and(|expiry| expiry <= now)
            || !input.resource_commitments.is_object()
        {
            return Err(AuthError::Denied);
        }
        let kind = match participant.kind() {
            ParticipantKind::Service => "service",
            ParticipantKind::Device => "device",
        };
        {
            let mut provided = Vec::new();
            for api in participant.implements() {
                let (generation, accepted): (u64, String) = connection
                    .query_row(
                        "SELECT generation,definition_json FROM auth_accepted_apis WHERE api_id=?1",
                        [api.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .ok_or(AuthError::Denied)?;
                if input.expected_api_generations.get(api.as_str()) != Some(&generation) {
                    return Err(AuthError::Conflict);
                }
                let accepted = compile_evidence(decode(&accepted)?)
                    .map_err(|error| AuthError::Invalid(error.to_string()))?;
                let implementation = graph.api(api).ok_or(AuthError::NotFound)?;
                let accepted = resolve_api(&accepted, api.as_str())?;
                if !trellis_idl::compare_implementation(implementation, accepted).compatible {
                    return Err(AuthError::Denied);
                }
                let implemented_actions = implementation
                    .definition()
                    .actions()
                    .keys()
                    .flat_map(|identity| {
                        action_identity(&ActionSelection {
                            action: identity.clone(),
                            direction: match identity.kind {
                                ActionKind::Rpc => InteractionDirection::Call,
                                ActionKind::Operation => InteractionDirection::Invoke,
                                ActionKind::Event => InteractionDirection::Publish,
                                ActionKind::Live => InteractionDirection::Subscribe,
                            },
                        })
                    })
                    .collect();
                provided.push(ProviderImplementation {
                    api_id: api.to_string(),
                    generation,
                    implementation_digest: api_digest(&graph, api)
                        .map_err(|error| AuthError::Invalid(error.to_string()))?,
                    implemented_actions,
                });
            }
            if input.expected_api_generations.len() != provided.len() {
                return Err(AuthError::Conflict);
            }
            for requested in required.iter().chain(&optional) {
                capability(connection, requested)?;
            }
            for package in &input.evidence.packages {
                connection.execute(
                    "INSERT OR IGNORE INTO \
                    auth_package_evidence(package_digest,accepted_at) \
                    VALUES(?1,?2)",
                    params![package.digest, now],
                )?;
            }
            let evidence_digest = digest(&input.evidence)?;
            connection.execute(
                "INSERT OR IGNORE INTO auth_package_evidence_documents VALUES(?1,?2,?3,?4)",
                params![
                    evidence_digest,
                    input.evidence.root_digest,
                    json(&input.evidence)?,
                    now
                ],
            )?;
            let unchanged: Option<u64> = existing
                .map(|deployment| {
                    connection
                        .query_row(
                            "SELECT installed_revision \
                            FROM auth_deployment_bindings \
                            WHERE deployment_id=?1 \
                            AND implementation_digest=?2",
                            params![deployment, participant_digest],
                            |row| row.get(0),
                        )
                        .optional()
                })
                .transpose()?
                .flatten();
            let revision: u64 = match unchanged {
                Some(revision) => revision,
                None => connection.query_row(
                    "SELECT coalesce(max(revision),0)+1 \
                    FROM auth_installed_participants \
                    WHERE participant_id=?1",
                    [&input.participant_id],
                    |row| row.get(0),
                )?,
            };
            if unchanged.is_none() {
                connection.execute(
                    "INSERT INTO auth_installed_participants(participant_id,revision,\
                    participant_kind,participant_digest,needs_digest,package_digest,\
                    evidence_digest,participant_path,companion_required,projection_json,\
                    installed_at,companion_request_id,companion_request_kind) \
                    VALUES(?1,?2,?3,?4,?5,?6,?7,?1,?10,?8,?9,?11,CASE \
                    WHEN ?11 IS NULL THEN NULL \
                    ELSE 'native' END)",
                    params![
                        input.participant_id,
                        revision,
                        kind,
                        participant_digest,
                        needs.digest(),
                        input.evidence.root_digest,
                        evidence_digest,
                        json(&input.evidence)?,
                        now,
                        participant
                            .companion()
                            .is_some_and(|companion| !companion.optional),
                        participant
                            .companion()
                            .map(|companion| companion.request.as_str())
                    ],
                )?;
            }
            let deployment_id = existing.map(str::to_owned).unwrap_or_else(id);
            if let Some(companion) = participant.companion() {
                let request = graph
                    .packages()
                    .values()
                    .find_map(|package| package.app_requests().get(&companion.request))
                    .ok_or(AuthError::NotFound)?;
                if request.kind() != trellis_idl::AppKind::Native {
                    return Err(AuthError::Invalid(
                        "device companions require a native OAuth request".into(),
                    ));
                }
                let mut required: Vec<_> = request
                    .uses()
                    .values()
                    .flat_map(|selection| selection.required_capabilities.iter())
                    .map(ToString::to_string)
                    .collect();
                if !request.states().is_empty() {
                    required.extend(
                        [
                            "trellis.state.v1:values.read",
                            "trellis.state.v1:values.write",
                            "trellis.state.v1:values.delete",
                        ]
                        .map(str::to_owned),
                    );
                    required.sort();
                    required.dedup();
                }
                let optional: Vec<_> = request
                    .uses()
                    .values()
                    .flat_map(|selection| selection.optional_capabilities.iter())
                    .map(ToString::to_string)
                    .collect();
                for name in required.iter().chain(&optional) {
                    capability(connection, name)?;
                }
                let client = format!("trellis.device.{deployment_id}.companion");
                let metadata = json(&serde_json::json!({
                    "grant_types":["urn:ietf:params:oauth:grant-type:device_code","refresh_token"],
                    "trellis_companion": {
                        "deploymentId":deployment_id,
                        "requestId":companion.request.as_str(),
                        "required":required,
                        "optional":optional,
                        "participantDigest":participant_digest
                    }
                }))?;
                // This registration is derived from the accepted deployment, not
                // caller-supplied OAuth metadata. It grants no user authority.
                connection.execute(
                    "INSERT INTO auth_oauth_clients(client_id,kind,display_name,\
                    redirect_uris_json,development,implied_capabilities_json,\
                    metadata_json,state,revision,created_at,updated_at) \
                    VALUES(?1,'native',?2,'[]',0,'[]',?3,'active',1,?4,?4) \
                    ON CONFLICT(client_id) \
                    DO UPDATE SET metadata_json=excluded.metadata_json,\
                    display_name=excluded.display_name,\
                    revision=auth_oauth_clients.revision+1,updated_at=excluded.updated_at \
                    WHERE auth_oauth_clients.metadata_json!=excluded.metadata_json",
                    params![client, format!("{title} companion"), metadata, now],
                )?;
            }
            for (name, definition) in participant.resources() {
                if let trellis_idl::ResourceDefinition::Kv {
                    history, ttl_ms, ..
                } = definition
                {
                    let saved: Option<String> = connection
                        .query_row(
                            "SELECT commitment_json \
                            FROM auth_resources \
                            WHERE owner_kind='deployment' \
                            AND owner_id=?1 AND participant_id=?2 \
                            AND kind='kv' AND local_name=?3",
                            params![deployment_id, input.participant_id, name.to_string()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(saved) = saved {
                        let saved: serde_json::Value = serde_json::from_str(&saved)?;
                        let old_history =
                            saved["history"].as_u64().ok_or(AuthError::Unavailable)?;
                        let old_ttl = saved["ttl_ms"].as_u64().ok_or(AuthError::Unavailable)?;
                        if *history < old_history
                            || (*ttl_ms != 0 && (old_ttl == 0 || *ttl_ms < old_ttl))
                        {
                            return Err(AuthError::Invalid("potential_data_loss".into()));
                        }
                    }
                }
            }
            let resource_identities = participant
                .resources()
                .iter()
                .map(|(name, definition)| {
                    let kind = match definition {
                        trellis_idl::ResourceDefinition::Kv { .. } => "kv",
                        trellis_idl::ResourceDefinition::Store { .. } => "store",
                        trellis_idl::ResourceDefinition::Consumer { .. } => "consumer",
                        trellis_idl::ResourceDefinition::Job { .. } => "job",
                        trellis_idl::ResourceDefinition::State { .. } => "state",
                    };
                    (kind.to_owned(), name.to_string())
                })
                .collect();
            super::resources::detach_deployment_resources(
                connection,
                &deployment_id,
                &resource_identities,
                now,
            )?;
            let mut resource_commitments = input.resource_commitments.clone();
            if let Some(kv) = resource_commitments
                .get_mut("kv")
                .and_then(serde_json::Value::as_object_mut)
            {
                kv.retain(|name, _| resource_identities.contains(&("kv".to_owned(), name.clone())));
            }
            if existing.is_none() {
                connection.execute(
                    "INSERT INTO auth_deployments(deployment_id,participant_id,participant_kind,\
                    state,expires_at,title) \
                    VALUES(?1,?2,?3,'active',?4,?5)",
                    params![
                        deployment_id,
                        input.participant_id,
                        kind,
                        input.expires_at,
                        title
                    ],
                )?;
            }
            connection.execute(
                "INSERT INTO auth_deployment_bindings(deployment_id,installed_revision,\
                implementation_digest,provided_apis_json,required_capabilities_json,\
                optional_capabilities_json,resource_commitments_json,revision) \
                VALUES(?1,?2,?3,?4,?5,?6,?7,1) \
                ON CONFLICT(deployment_id) \
                DO UPDATE SET installed_revision=excluded.installed_revision,\
                implementation_digest=excluded.implementation_digest,\
                provided_apis_json=excluded.provided_apis_json,\
                required_capabilities_json=excluded.required_capabilities_json,\
                optional_capabilities_json=excluded.optional_capabilities_json,\
                resource_commitments_json=excluded.resource_commitments_json,\
                revision=auth_deployment_bindings.revision+1",
                params![
                    deployment_id,
                    revision,
                    participant_digest,
                    json(&provided)?,
                    json(&required)?,
                    json(&optional)?,
                    json(&resource_commitments)?
                ],
            )?;
            if unchanged.is_some() {
                // Identical declarations retain the same allocations. Carry only
                // ready evidence verified at the immediately preceding binding;
                // changed, pending, detached, or already stale evidence stays fenced.
                connection.execute(
                    "UPDATE auth_resources \
                    SET binding_revision=binding_revision+1 \
                    WHERE owner_kind='deployment' \
                    AND owner_id=?1 AND state='ready' \
                    AND binding_revision=(SELECT revision-1 \
                    FROM auth_deployment_bindings \
                    WHERE deployment_id=?1)",
                    [&deployment_id],
                )?;
            }
            if kind == "service"
                && participant.resources().values().any(|resource| {
                    matches!(resource, trellis_idl::ResourceDefinition::Consumer { .. })
                })
            {
                let cap = capability(connection, CONSUMER_RUNTIME_CAPABILITY)?;
                let mut statement = connection
                    .prepare("SELECT principal_id FROM auth_instances WHERE deployment_id=?1")?;
                let principals = statement
                    .query_map([&deployment_id], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                for principal in principals {
                    connection.execute(
                        "INSERT OR IGNORE INTO auth_direct_capabilities(grant_id,principal_id,\
                        capability_id,capability_generation,revision,expires_at,created_at) \
                        VALUES(?1,?2,?3,?4,1,?5,?6)",
                        params![
                            id(),
                            principal,
                            CONSUMER_RUNTIME_CAPABILITY,
                            cap.identity_generation.get(),
                            input.expires_at,
                            now
                        ],
                    )?;
                }
            }
            Ok(deployment_id)
        }
    }

    /// Provision one immutable credential/principal/instance aggregate against
    /// the exact installed deployment revision and approved capability selection.
    pub(super) fn provision_instance_in_transaction(
        connection: &crate::platform::auth::sqlite::Connection,
        deployment: &str,
        public_key: Option<&str>,
        title: &str,
        now: i64,
    ) -> Result<ProvisionedDeployment, AuthError> {
        if title.trim().is_empty() {
            return Err(AuthError::Invalid("instance title is required".into()));
        }
        let identity_key_id = public_key
            .map(|public_key| {
                let bytes = URL_SAFE_NO_PAD
                    .decode(public_key)
                    .map_err(|_| AuthError::Denied)?;
                let raw: [u8; 32] = bytes.try_into().map_err(|_| AuthError::Denied)?;
                if VerifyingKey::from_bytes(&raw)
                    .map_err(|_| AuthError::Denied)?
                    .is_weak()
                    || URL_SAFE_NO_PAD.encode(raw) != public_key
                {
                    return Err(AuthError::Denied);
                }
                Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(raw)))
            })
            .transpose()?;
        let (participant, kind, revision, required, optional, expiry): (
            String,
            String,
            u64,
            String,
            String,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT d.participant_id,d.participant_kind,b.installed_revision,\
                b.required_capabilities_json,b.optional_capabilities_json,d.expires_at \
                FROM auth_deployments d \
                JOIN auth_deployment_bindings b USING(deployment_id) \
                WHERE deployment_id=?1 \
                AND d.state='active' \
                AND (d.expires_at IS NULL \
                OR d.expires_at>?2)",
                params![deployment, now],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AuthError::Denied)?;
        let mut required: Vec<String> = decode(&required)?;
        if kind == "service" || kind == "device" {
            let evidence: String = connection.query_row(
                "SELECT projection_json \
                FROM auth_installed_participants \
                WHERE participant_id=?1 \
                AND revision=?2",
                params![participant, revision],
                |row| row.get(0),
            )?;
            let graph = trellis_idl::compile_evidence(decode(&evidence)?)
                .map_err(|error| AuthError::Invalid(error.to_string()))?;
            let installed = graph
                .packages()
                .values()
                .flat_map(|package| package.participants().values())
                .find(|value| value.identity().as_str() == participant);
            if kind == "service"
                && installed.is_some_and(|value| {
                    value.resources().values().any(|resource| {
                        matches!(resource, trellis_idl::ResourceDefinition::Job { .. })
                    })
                })
            {
                required.push(JOB_HISTORY_CAPABILITY.into());
            }
            if kind == "service"
                && installed.is_some_and(|value| {
                    value.resources().values().any(|resource| {
                        matches!(resource, trellis_idl::ResourceDefinition::Consumer { .. })
                    })
                })
            {
                required.push(CONSUMER_RUNTIME_CAPABILITY.into());
            }
            if kind == "device"
                && installed.is_some_and(|value| {
                    value.resources().values().any(|resource| {
                        matches!(resource, trellis_idl::ResourceDefinition::State { .. })
                    })
                })
            {
                required.extend(
                    [
                        "trellis.state.v1:values.read",
                        "trellis.state.v1:values.write",
                        "trellis.state.v1:values.delete",
                    ]
                    .map(str::to_owned),
                );
            }
        }
        let optional: Vec<String> = decode(&optional)?;
        let result = ProvisionedDeployment {
            participant_id: participant,
            principal_id: id(),
            deployment_id: deployment.into(),
            instance_id: id(),
            identity_key_id: identity_key_id.unwrap_or_default(),
        };
        let known: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_provisioned_identities WHERE identity_key_id=?1)",
            [&result.identity_key_id],
            |row| row.get(0),
        )?;
        if known {
            return Err(AuthError::Conflict);
        }
        connection.execute(
            "INSERT INTO auth_principals(principal_id,kind,state,created_at,\
            updated_at,version) \
            VALUES(?1,?2,'active',?3,?3,1)",
            params![result.principal_id, kind, now],
        )?;
        if kind == "device" {
            connection.execute(
                "INSERT INTO auth_devices(principal_id,deployment_id,state,created_at,\
                updated_at,version) \
                VALUES(?1,?2,'active',?3,?3,1)",
                params![result.principal_id, deployment, now],
            )?;
        }
        connection.execute(
            "INSERT INTO auth_instances(instance_id,deployment_id,principal_id,\
            installed_revision,state,created_at,updated_at,version,title) \
            VALUES(?1,?2,?3,?4,'active',?5,?5,1,?6)",
            params![
                result.instance_id,
                deployment,
                result.principal_id,
                revision,
                now,
                title
            ],
        )?;
        if let Some(public_key) = public_key {
            connection.execute(
                "INSERT INTO auth_provisioned_identities(identity_key_id,principal_id,\
                deployment_id,instance_id,participant_id,kind,identity_public_key,\
                state,created_at) \
                VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8)",
                params![
                    result.identity_key_id,
                    result.principal_id,
                    deployment,
                    result.instance_id,
                    result.participant_id,
                    kind,
                    public_key,
                    now
                ],
            )?;
        } else {
            if kind != "device" {
                return Err(AuthError::Denied);
            }
            connection.execute(
                "UPDATE auth_instances SET state='disabled' WHERE instance_id=?1",
                [&result.instance_id],
            )?;
            connection.execute(
                "UPDATE auth_devices SET state='pending' WHERE principal_id=?1",
                [&result.principal_id],
            )?;
        }
        native_health_capability(connection)?;
        let requested: std::collections::BTreeSet<_> = required
            .iter()
            .chain(&optional)
            .map(String::as_str)
            .chain(std::iter::once(SELF_HEALTH_CAPABILITY))
            .chain((kind == "service").then_some(SERVICE_UPDATE_CAPABILITY))
            .collect();
        for requested in requested {
            let cap = capability(connection, requested)?;
            connection.execute(
                "INSERT INTO auth_direct_capabilities(grant_id,principal_id,\
                capability_id,capability_generation,revision,expires_at,created_at) \
                VALUES(?1,?2,?3,?4,1,?5,?6)",
                params![
                    id(),
                    result.principal_id,
                    requested,
                    cap.identity_generation.get(),
                    expiry,
                    now
                ],
            )?;
        }
        Ok(result)
    }
}
