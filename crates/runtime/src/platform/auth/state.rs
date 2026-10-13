//! State representation registration in the existing resource catalog.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trellis_protocol::SessionBinding;
use trellis_runtime_apis::types::{
    StateRegisterRequest, StateRegisterResponse, StateRegistrationConflict, StateRevision,
};

use super::{
    sqlite::{decode, digest, id},
    AuthError, Mutation, SqliteAuthorizationStore,
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StateDeclaration {
    pub(crate) resource_id: String,
    pub(crate) resource_name: String,
    pub(crate) schema: Value,
    pub(crate) representation_version: u32,
    pub(crate) accepted_versions: BTreeMap<u32, Value>,
}

impl SqliteAuthorizationStore {
    pub(crate) fn state_namespace(
        principal: &str,
        binding: &SessionBinding,
        resource_name: &str,
    ) -> Result<(String, Value), AuthError> {
        let namespace = match binding {
            SessionBinding::Browser { client_id, origin } => {
                json!({"kind":"browser","clientId":client_id,"origin":origin})
            }
            SessionBinding::Native { client_id, .. } => {
                json!({"kind":"native","clientId":client_id})
            }
            SessionBinding::Device { participant_id, .. } => {
                json!({"kind":"device","participantId":participant_id})
            }
            SessionBinding::Service { .. } => return Err(AuthError::Denied),
        };
        let resource_id = digest(&("trellis.state", principal, &namespace, resource_name))?;
        Ok((resource_id, namespace))
    }

    pub(crate) fn state_declaration(
        &self,
        resource_id: &str,
    ) -> Result<StateDeclaration, AuthError> {
        self.transaction(|connection| {
            let declaration: Option<(String,String,String)> = connection.query_row(
                "SELECT commitment_json,physical_id,allocation_id FROM auth_resources WHERE resource_id=?1 AND kind='state' AND state='ready'",
                [resource_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            ).optional()?;
            let (commitment, physical, allocation) = declaration.ok_or(AuthError::NotFound)?;
            if physical != format!("{resource_id}.{allocation}") { return Err(AuthError::Denied); }
            let mut declaration: StateDeclaration = decode(&commitment)?;
            declaration.resource_id = physical;
            Ok(declaration)
        })
    }

    pub(crate) fn device_state_declaration(
        &self,
        principal: &str,
        binding: &SessionBinding,
        bindings_digest: Option<&str>,
        resource_name: &str,
    ) -> Result<StateDeclaration, AuthError> {
        let SessionBinding::Device {
            deployment_id,
            instance_id,
            participant_id,
        } = binding
        else {
            return Err(AuthError::Denied);
        };
        self.transaction(|connection| {
            let resources: Option<String> = connection.query_row(
                "SELECT b.resource_commitments_json FROM auth_instances i JOIN auth_deployments d USING(deployment_id) JOIN auth_deployment_bindings b ON b.deployment_id=i.deployment_id AND b.installed_revision=i.installed_revision JOIN auth_devices v ON v.principal_id=i.principal_id AND v.deployment_id=i.deployment_id WHERE i.principal_id=?1 AND i.instance_id=?2 AND i.deployment_id=?3 AND d.participant_id=?4 AND i.state='active' AND d.state='active' AND v.state='active'",
                params![principal,instance_id,deployment_id,participant_id], |row| row.get(0),
            ).optional()?;
            let resources: Value = decode(&resources.ok_or(AuthError::Denied)?)?;
            if bindings_digest != Some(digest(&resources)?.as_str()) { return Err(AuthError::Denied); }
            let resource_id = resources.get("state").and_then(|state| state.get(resource_name)).and_then(Value::as_str).ok_or(AuthError::Denied)?;
            let commitment: Option<(String,String,String)> = connection.query_row(
                "SELECT commitment_json,physical_id,allocation_id FROM auth_resources WHERE resource_id=?1 AND owner_kind='deployment' AND owner_id=?2 AND participant_id=?3 AND local_name=?4 AND kind='state' AND state='ready'",
                params![resource_id,deployment_id,participant_id,resource_name], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            ).optional()?;
            let (commitment, physical, allocation) = commitment.ok_or(AuthError::Denied)?;
            if physical != format!("{resource_id}.{allocation}") { return Err(AuthError::Denied); }
            let mut declaration: StateDeclaration = decode(&commitment)?;
            // The installed schema belongs to the deployment; values belong to this device.
            declaration.resource_id = format!("{physical}.{}", Self::state_namespace(principal,binding,resource_name)?.0);
            Ok(declaration)
        })
    }

    pub(crate) fn register_state(
        &self,
        principal: &str,
        binding: &SessionBinding,
        input: &StateRegisterRequest,
        now: i64,
    ) -> Result<Result<StateRegisterResponse, StateRegistrationConflict>, AuthError> {
        if !matches!(
            binding,
            SessionBinding::Browser { .. } | SessionBinding::Native { .. }
        ) {
            return Err(AuthError::Denied);
        }
        let (resource_id, namespace) =
            Self::state_namespace(principal, binding, &input.resource_name.0)?;
        let mut schemas = BTreeMap::new();
        for representation in std::iter::once(&input.representation).chain(&input.accepts) {
            if representation.schema.0.len() > 65_536 || representation.version.0 == 0 {
                return Err(AuthError::Invalid("invalid State representation".into()));
            }
            let schema: Value = serde_json::from_slice(&representation.schema.0)?;
            // User schemas cannot cause remote retrieval during registration or reads.
            let mut pending = vec![&schema];
            while let Some(value) = pending.pop() {
                match value {
                    Value::Object(fields) => {
                        if ["$ref", "$dynamicRef", "$recursiveRef"].iter().any(|key| {
                            fields
                                .get(*key)
                                .and_then(Value::as_str)
                                .is_some_and(|reference| !reference.starts_with('#'))
                        }) {
                            return Err(AuthError::Invalid(
                                "State schemas require local references".into(),
                            ));
                        }
                        pending.extend(fields.values());
                    }
                    Value::Array(values) => pending.extend(values),
                    _ => {}
                }
            }
            jsonschema::validator_for(&schema)
                .map_err(|_| AuthError::Invalid("invalid State schema".into()))?;
            if schemas.insert(representation.version.0, schema).is_some() {
                return Err(AuthError::Invalid(
                    "duplicate State representation version".into(),
                ));
            }
        }
        let schema = schemas
            .remove(&input.representation.version.0)
            .ok_or(AuthError::Unavailable)?;
        let declaration = StateDeclaration {
            resource_id: resource_id.clone(),
            resource_name: input.resource_name.0.clone(),
            schema,
            representation_version: input.representation.version.0,
            accepted_versions: schemas,
        };
        let identity = Mutation {
            actor: principal.into(),
            request_id: input.idempotency_key.to_string(),
            request_digest: digest(&(&namespace, input))?,
        };
        self.mutate(&identity, "state.register", &(&namespace, input), now, |connection| {
            let existing: Option<(String,u64)> = connection.query_row(
                "SELECT commitment_json,revision FROM auth_resources WHERE resource_id=?1 AND state='ready'",
                [&resource_id], |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional()?;
            if let Some((previous, revision)) = &existing {
                let previous: StateDeclaration = decode(previous)?;
                if serde_json::to_value(&previous)? == serde_json::to_value(&declaration)? {
                    return Ok(Ok(StateRegisterResponse { revision: StateRevision(revision.to_string()), extra:Default::default() }));
                }
            }
            let mut history = connection.prepare("SELECT commitment_json FROM auth_resource_history WHERE resource_id=?1")?;
            for previous in history.query_map([&resource_id], |row| row.get::<_, String>(0))? {
                let previous: StateDeclaration = decode(&previous?)?;
                for (version, schema) in std::iter::once((&declaration.representation_version, &declaration.schema)).chain(&declaration.accepted_versions) {
                    let old = if *version == previous.representation_version {Some(&previous.schema)} else {previous.accepted_versions.get(version)};
                    if old.is_some_and(|old| old != schema) {return Err(AuthError::Invalid("State representation versions are immutable".into()));}
                }
            }
            let revision = existing.as_ref().map_or(0, |(_, revision)| *revision);
            if input.expected_revision.0 != revision.to_string() { return Ok(Err(StateRegistrationConflict {current_revision:StateRevision(revision.to_string()),extra:Default::default()})); }
            let next = revision.checked_add(1).ok_or(AuthError::Unavailable)?;
            let encoded = serde_json::to_string(&declaration)?;
            if existing.is_some() {
                connection.execute("UPDATE auth_resources SET commitment_json=?2,revision=?3,updated_at=?4 WHERE resource_id=?1",params![resource_id,encoded,next,now])?;
            } else {
                let allocation = id();
                connection.execute("INSERT INTO auth_resources(resource_id,allocation_id,owner_kind,owner_id,namespace_json,kind,local_name,commitment_json,physical_id,state,binding_revision,revision,created_at,updated_at) VALUES(?1,?2,'user',?3,?4,'state',?5,?6,?7,'ready',1,1,?8,?8)", params![resource_id,allocation,principal,serde_json::to_string(&namespace)?,declaration.resource_name,encoded,format!("{resource_id}.{allocation}"),now])?;
            }
            connection.execute("INSERT INTO auth_resource_history(resource_id,revision,commitment_json,state,changed_at) VALUES(?1,?2,?3,'ready',?4)",params![resource_id,next,encoded,now])?;
            Ok(Ok(StateRegisterResponse {revision:StateRevision(next.to_string()),extra:Default::default()}))
        })
    }
}
