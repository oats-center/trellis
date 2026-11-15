//! Configuration a participant declares and Trellis keeps.
//!
//! See `design/core/config-patterns.md`. A service or device declares typed
//! fields in IDL. Administrators store values per deployment, with optional
//! overrides per instance; every write is a complete new revision of that
//! target. Secret values are sealed with the installation's storage key and
//! only the participant itself reads them back.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use trellis_idl::{compile_evidence, ConfigFieldDefinition, PackageGraph};
use trellis_rs::client::EventDescriptor;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo},
    apis::trellis_auth_v1::{events, Provider},
};

use super::oauth::storage::OAuthStorage;
use super::revocation::enqueue_effect;
use super::rpc::{caller, mutation, rpc_error};
use super::sqlite::{decode, digest, json, timestamp, Connection};
use super::{blocking, Administration, AuthError, Mutation, SqliteAuthorizationStore};

/// Register the administrator surface and the participant's own read.
pub(crate) fn register_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    engine: Arc<super::OAuthEngine>,
) {
    let get = Arc::clone(&engine);
    provider.register_config_get(move |context, input| {
        let engine = Arc::clone(&get);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                let storage = engine.server.store();
                storage.authority.transaction(|connection| {
                    require_any(
                        connection,
                        &actor,
                        &[
                            Administration::ReadConfig,
                            Administration::ManageConfig,
                            Administration::ManageConfigSecrets,
                        ],
                    )?;
                    let target = Target::resolve(connection, &input.target)?;
                    response(connection, storage, &target)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let set = Arc::clone(&engine);
    provider.register_config_set(move |context, input| {
        let engine = Arc::clone(&set);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || set_values(engine.server.store(), &identity, &input))
                .await
                .map_err(rpc_error)
        }
    });
    let list = Arc::clone(&engine);
    provider.register_config_revisions_list(move |context, input| {
        let engine = Arc::clone(&list);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                engine.server.store().authority.transaction(|connection| {
                    require_any(
                        connection,
                        &actor,
                        &[
                            Administration::ReadConfig,
                            Administration::ManageConfig,
                            Administration::ManageConfigSecrets,
                        ],
                    )?;
                    let target = Target::resolve(connection, &input.target)?;
                    revisions(connection, &target, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let delete = Arc::clone(&engine);
    provider.register_config_revisions_delete(move |context, input| {
        let engine = Arc::clone(&delete);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || {
                let storage = engine.server.store();
                storage.authority.mutate(
                    &identity,
                    "rpc.config.revisions.delete",
                    &input,
                    crate::clock::unix_now(),
                    |connection| {
                        require_any(
                            connection,
                            &identity.actor,
                            &[
                                Administration::ManageConfig,
                                Administration::ManageConfigSecrets,
                            ],
                        )?;
                        let target = Target::resolve(connection, &input.target)?;
                        let key = target.key();
                        let current = current(connection, &key)?.ok_or(AuthError::NotFound)?;
                        let revision = input.revision.0;
                        if revision == current.revision {
                            return Err(AuthError::Invalid(
                                "the current revision cannot be deleted".into(),
                            ));
                        }
                        let deleted = connection.execute(
                            "DELETE FROM auth_config_revisions \
                            WHERE target_key=?1 AND revision=?2",
                            params![key, revision],
                        )?;
                        if deleted == 0 {
                            return Err(AuthError::NotFound);
                        }
                        response(connection, storage, &target)
                    },
                )
            })
            .await
            .map_err(rpc_error)
        }
    });
    let rollback = Arc::clone(&engine);
    provider.register_config_rollback(move |context, input| {
        let engine = Arc::clone(&rollback);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || roll_back(engine.server.store(), &identity, &input))
                .await
                .map_err(rpc_error)
        }
    });
    let declare = Arc::clone(&engine);
    provider.register_config_apps_declare(move |context, input| {
        let engine = Arc::clone(&declare);
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || {
                let storage = engine.server.store();
                let now = crate::clock::unix_now();
                storage.authority.mutate(
                    &identity,
                    "rpc.config.apps.declare",
                    &input,
                    now,
                    |connection| {
                        SqliteAuthorizationStore::require_administration(
                            connection,
                            &identity.actor,
                            Administration::ManageConfig,
                        )?;
                        let target = Target::resolve(
                            connection,
                            &serde_json::from_value(json!({ "clientId": input.client_id }))?,
                        )?;
                        let evidence: trellis_idl::PackageEvidence =
                            serde_json::from_value(input.package_evidence.clone())?;
                        let graph = compile_evidence(evidence)
                            .map_err(|error| AuthError::Invalid(error.to_string()))?;
                        app_fields(&graph, &input.request_id)?;
                        // Connected users of the app reread only when told, so a
                        // declaration that changes what they read is announced.
                        let stored = current(connection, &target.key())?;
                        let empty = Map::new();
                        let settings = stored.as_ref().map_or(&empty, |stored| &stored.values);
                        let read = |connection: &Connection| -> Result<Vec<_>, AuthError> {
                            let declared = Declared::of_target(connection, &target)?;
                            Ok(effective(&declared, &[(settings, &empty, "client")])?
                                .into_iter()
                                .map(|(name, value)| (name, value.value))
                                .collect())
                        };
                        // A declaration that no longer reads is replaced, not kept.
                        let before = read(connection).ok();
                        connection.execute(
                            "INSERT INTO auth_client_config_declarations(client_id,request_id,\
                            evidence_json,declared_by,declared_at) VALUES(?1,?2,?3,?4,?5) \
                            ON CONFLICT(client_id) DO UPDATE SET request_id=excluded.request_id,\
                            evidence_json=excluded.evidence_json,declared_by=excluded.declared_by,\
                            declared_at=excluded.declared_at",
                            params![
                                input.client_id.to_string(),
                                input.request_id,
                                json(&input.package_evidence)?,
                                identity.actor,
                                now
                            ],
                        )?;
                        if Some(read(connection)?) != before {
                            let revision = stored.as_ref().map(|stored| stored.revision);
                            announce(connection, &target, revision, now)?;
                        }
                        response(connection, storage, &target)
                    },
                )
            })
            .await
            .map_err(rpc_error)
        }
    });
    let app = Arc::clone(&engine);
    provider.register_config_app_get(move |context, _input| {
        let engine = Arc::clone(&app);
        async move {
            caller(&context, None).map_err(rpc_error)?;
            let binding = context
                .request()
                .verified_request()
                .map(|request| request.caller().binding.clone());
            let client = match binding {
                Some(trellis_protocol::SessionBinding::Browser { client_id, .. })
                | Some(trellis_protocol::SessionBinding::Native { client_id, .. }) => client_id,
                _ => return Err(rpc_error(AuthError::Denied)),
            };
            blocking(move || {
                engine.server.store().authority.transaction(|connection| {
                    let declared = Declared::client(connection, &client)?;
                    let key = format!("client:{client}");
                    let stored = current(connection, &key)?;
                    let empty = Map::new();
                    let settings = stored.as_ref().map_or(&empty, |stored| &stored.values);
                    let values = effective(&declared, &[(settings, &empty, "client")])?
                        .into_iter()
                        .filter_map(|(name, value)| value.value.map(|value| (name, value)))
                        .collect::<Map<_, _>>();
                    Ok(serde_json::from_value(json!({
                        "revision": stored.as_ref().map(|stored| stored.revision),
                        "values": values,
                    }))?)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let own = Arc::clone(&engine);
    provider.register_config_own_get(move |context, input| {
        let engine = Arc::clone(&own);
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || own_config(engine.server.store(), &actor, input.at_connect))
                .await
                .map_err(rpc_error)
        }
    });
}

fn require_any(
    connection: &Connection,
    actor: &str,
    any: &[Administration],
) -> Result<(), AuthError> {
    for administration in any {
        if SqliteAuthorizationStore::require_administration(connection, actor, *administration)
            .is_ok()
        {
            return Ok(());
        }
    }
    Err(AuthError::Denied)
}

/// Whose configuration a request names.
enum Target {
    Deployment {
        deployment: String,
    },
    Instance {
        deployment: String,
        instance: String,
    },
    Client {
        client: String,
    },
}

impl Target {
    fn resolve(connection: &Connection, target: &AuthConfigTarget) -> Result<Self, AuthError> {
        if let Some(client) = &target.client_id {
            if target.deployment_id.is_some() || target.instance_id.is_some() {
                return Err(AuthError::Invalid(
                    "name either an app's client or a deployment, not both".into(),
                ));
            }
            let client = client.to_string();
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients \
                WHERE client_id=?1 AND state!='deleted')",
                [&client],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(AuthError::NotFound);
            }
            return Ok(Self::Client { client });
        }
        let deployment = target
            .deployment_id
            .as_ref()
            .ok_or_else(|| AuthError::Invalid("name the deployment to configure".into()))?
            .to_string();
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_deployments \
            WHERE deployment_id=?1 AND state!='revoked')",
            [&deployment],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AuthError::NotFound);
        }
        let Some(instance) = target.instance_id.as_ref().map(ToString::to_string) else {
            return Ok(Self::Deployment { deployment });
        };
        let belongs: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM auth_instances \
            WHERE instance_id=?1 AND deployment_id=?2 AND state!='revoked')",
            params![instance, deployment],
            |row| row.get(0),
        )?;
        if !belongs {
            return Err(AuthError::NotFound);
        }
        Ok(Self::Instance {
            deployment,
            instance,
        })
    }

    fn key(&self) -> String {
        match self {
            Self::Deployment { deployment } => format!("deployment:{deployment}"),
            Self::Instance { instance, .. } => format!("instance:{instance}"),
            Self::Client { client } => format!("client:{client}"),
        }
    }

    /// The key whose values sit under any instance overrides.
    fn base_key(&self) -> String {
        match self {
            Self::Deployment { deployment } | Self::Instance { deployment, .. } => {
                format!("deployment:{deployment}")
            }
            Self::Client { .. } => self.key(),
        }
    }

    fn deployment(&self) -> Option<&str> {
        match self {
            Self::Deployment { deployment } | Self::Instance { deployment, .. } => Some(deployment),
            Self::Client { .. } => None,
        }
    }

    fn instance(&self) -> Option<&str> {
        match self {
            Self::Instance { instance, .. } => Some(instance),
            Self::Deployment { .. } | Self::Client { .. } => None,
        }
    }

    fn client(&self) -> Option<&str> {
        match self {
            Self::Client { client } => Some(client),
            Self::Deployment { .. } | Self::Instance { .. } => None,
        }
    }

    fn wire(&self) -> Result<AuthConfigTarget, AuthError> {
        Ok(serde_json::from_value(json!({
            "deploymentId": self.deployment(),
            "instanceId": self.instance(),
            "clientId": self.client(),
        }))?)
    }
}

/// The fields one participant revision declares, with the graph their types live in.
struct Declared {
    graph: Option<Arc<PackageGraph>>,
    fields: BTreeMap<String, ConfigFieldDefinition>,
}

impl Declared {
    fn of(connection: &Connection, participant: &str, revision: u64) -> Result<Self, AuthError> {
        let evidence: String = connection.query_row(
            "SELECT projection_json FROM auth_installed_participants \
            WHERE participant_id=?1 AND revision=?2",
            params![participant, revision],
            |row| row.get(0),
        )?;
        let graph = compile_evidence(decode(&evidence)?)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let fields = graph
            .packages()
            .values()
            .flat_map(|package| package.participants().values())
            .find(|definition| definition.identity().as_str() == participant)
            .ok_or(AuthError::Unavailable)?
            .config()
            .clone();
        Ok(Self {
            graph: Some(graph),
            fields,
        })
    }

    /// The fields of the build a deployment currently has installed. A
    /// deployment no build has been applied to declares nothing yet.
    fn installed(connection: &Connection, deployment: &str) -> Result<Self, AuthError> {
        let installed: Option<(String, u64)> = connection
            .query_row(
                "SELECT d.participant_id,b.installed_revision \
                FROM auth_deployments d JOIN auth_deployment_bindings b USING(deployment_id) \
                WHERE d.deployment_id=?1",
                [deployment],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match installed {
            Some((participant, revision)) => Self::of(connection, &participant, revision),
            None => Ok(Self {
                graph: None,
                fields: BTreeMap::new(),
            }),
        }
    }

    /// The fields an administrator attached to an app's client, if any.
    fn client(connection: &Connection, client: &str) -> Result<Self, AuthError> {
        let declared: Option<(String, String)> = connection
            .query_row(
                "SELECT request_id,evidence_json FROM auth_client_config_declarations \
                WHERE client_id=?1",
                [client],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((request, evidence)) = declared else {
            return Ok(Self {
                graph: None,
                fields: BTreeMap::new(),
            });
        };
        let graph = compile_evidence(decode(&evidence)?)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let fields = app_fields(&graph, &request)?;
        Ok(Self {
            graph: Some(graph),
            fields,
        })
    }

    fn of_target(connection: &Connection, target: &Target) -> Result<Self, AuthError> {
        match target {
            Target::Deployment { deployment } | Target::Instance { deployment, .. } => {
                Self::installed(connection, deployment)
            }
            Target::Client { client } => Self::client(connection, client),
        }
    }

    fn field(&self, name: &str) -> Result<&ConfigFieldDefinition, AuthError> {
        self.fields
            .get(name)
            .ok_or_else(|| AuthError::Invalid(format!("this build has no setting named {name}")))
    }

    /// The JSON Schema every value of `name` must satisfy.
    fn schema(&self, name: &str) -> Result<Value, AuthError> {
        let field = self.field(name)?;
        let graph = self.graph.as_deref().ok_or(AuthError::Unavailable)?;
        trellis_idl::expression_json_schema(graph, &field.ty)
            .map_err(|error| AuthError::Invalid(error.to_string()))
    }

    /// Why `value` cannot be stored for `name`, if it cannot.
    fn problem(&self, name: &str, value: &Value) -> Result<Option<String>, AuthError> {
        let schema = self.schema(name)?;
        let validator = jsonschema::validator_for(&schema)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let problem = validator
            .iter_errors(value)
            .next()
            .map(|error| format!("{name}: {error}"));
        Ok(problem)
    }

    fn check(&self, name: &str, value: &Value) -> Result<(), AuthError> {
        match self.problem(name, value)? {
            Some(problem) => Err(AuthError::Invalid(problem)),
            None => Ok(()),
        }
    }
}

/// The settings one app request in `graph` declares.
fn app_fields(
    graph: &PackageGraph,
    request: &str,
) -> Result<BTreeMap<String, ConfigFieldDefinition>, AuthError> {
    graph
        .packages()
        .values()
        .flat_map(|package| package.app_requests().values())
        .find(|definition| definition.identity().as_str() == request)
        .map(|definition| definition.config().clone())
        .ok_or_else(|| AuthError::Invalid(format!("the package declares no app named {request}")))
}

/// One stored revision of a target: its settings and sealed secrets.
struct Stored {
    revision: u32,
    values: Map<String, Value>,
    sealed: Option<Vec<u8>>,
}

fn stored(
    connection: &Connection,
    key: &str,
    revision: Option<u32>,
) -> Result<Option<Stored>, AuthError> {
    connection
        .query_row(
            "SELECT revision,values_json,sealed_secrets FROM auth_config_revisions \
            WHERE target_key=?1 AND (?2 IS NULL OR revision=?2) \
            ORDER BY revision DESC LIMIT 1",
            params![key, revision],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?, row.get(2)?)),
        )
        .optional()?
        .map(|(revision, values, sealed)| {
            Ok(Stored {
                revision,
                values: decode(&values)?,
                sealed,
            })
        })
        .transpose()
}

fn current(connection: &Connection, key: &str) -> Result<Option<Stored>, AuthError> {
    stored(connection, key, None)
}

/// Secrets are bound to their target, never to one revision, so a rollback
/// can carry an earlier revision's sealed values forward unchanged.
fn open_secrets(
    storage: &OAuthStorage,
    key: &str,
    sealed: Option<&[u8]>,
) -> Result<Map<String, Value>, AuthError> {
    sealed
        .map(|sealed| {
            storage
                .decode("configSecrets", key, sealed)
                .map_err(|_| AuthError::Unavailable)
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn seal_secrets(
    storage: &OAuthStorage,
    key: &str,
    secrets: &Map<String, Value>,
) -> Result<Option<Vec<u8>>, AuthError> {
    if secrets.is_empty() {
        return Ok(None);
    }
    storage
        .encode("configSecrets", key, secrets)
        .map(Some)
        .map_err(|_| AuthError::Unavailable)
}

/// One source of stored values: settings, secrets, and where they come from.
type Layer<'a> = (&'a Map<String, Value>, &'a Map<String, Value>, &'static str);

/// A field's value after instance overrides, deployment values, and defaults.
struct Effective {
    value: Option<Value>,
    source: &'static str,
    invalid: Option<String>,
}

/// Each layer is (settings, secrets, source), most specific first. A stored
/// value that no longer matches the build's type is skipped and reported.
fn effective(
    declared: &Declared,
    layers: &[Layer<'_>],
) -> Result<BTreeMap<String, Effective>, AuthError> {
    let mut result = BTreeMap::new();
    for (name, field) in &declared.fields {
        let mut found = None;
        let mut invalid = None;
        for (settings, secrets, source) in layers {
            let stored = if field.secret { secrets } else { settings };
            let Some(value) = stored.get(name) else {
                continue;
            };
            match declared.problem(name, value)? {
                None => {
                    found = Some((value.clone(), *source));
                    break;
                }
                Some(problem) => invalid = invalid.or(Some(problem)),
            }
        }
        let (value, source) = match (found, &field.default) {
            (Some((value, source)), _) => (Some(value), source),
            (None, Some(default)) => (Some(default.clone()), "default"),
            (None, None) => (None, "unset"),
        };
        result.insert(
            name.clone(),
            Effective {
                value,
                source,
                invalid,
            },
        );
    }
    Ok(result)
}

/// The administrator's view of a target: fields with settings shown and
/// secrets reduced to whether they are set, plus each instance's connect state.
fn response(
    connection: &Connection,
    storage: &OAuthStorage,
    target: &Target,
) -> Result<AuthConfigResponse, AuthError> {
    let declared = Declared::of_target(connection, target)?;
    let deployment_key = target.base_key();
    let deployment = current(connection, &deployment_key)?;
    let instance = match target.instance() {
        Some(_) => current(connection, &target.key())?,
        None => None,
    };
    let empty = Map::new();
    let deployment_secrets = open_secrets(
        storage,
        &deployment_key,
        deployment
            .as_ref()
            .and_then(|stored| stored.sealed.as_deref()),
    )?;
    let instance_secrets = open_secrets(
        storage,
        &target.key(),
        instance
            .as_ref()
            .and_then(|stored| stored.sealed.as_deref()),
    )?;
    let mut layers = Vec::new();
    if let Some(stored) = &instance {
        layers.push((&stored.values, &instance_secrets, "instance"));
    }
    layers.push((
        deployment.as_ref().map_or(&empty, |stored| &stored.values),
        &deployment_secrets,
        "deployment",
    ));
    let effective = effective(&declared, &layers)?;
    let mut fields = Vec::new();
    let mut missing = Vec::new();
    for (name, field) in &declared.fields {
        let value = &effective[name];
        if field.default.is_none() && value.value.is_none() {
            missing.push(name.clone());
        }
        let mut entry = json!({
            "name": name,
            "title": field.docs.title,
            "description": field.docs.description,
            "schema": declared.schema(name)?,
            "required": field.default.is_none(),
            "secret": field.secret,
            "restart": field.restart,
            "isSet": matches!(value.source, "instance" | "deployment" | "client"),
            "source": value.source,
        });
        // Absent and JSON null differ for `json` fields, so absent keys stay absent.
        if let Some(default) = &field.default {
            entry["default"] = default.clone();
        }
        if let (false, Some(current)) = (field.secret, &value.value) {
            entry["value"] = current.clone();
        }
        if let Some(invalid) = &value.invalid {
            entry["invalid"] = json!(invalid);
        }
        fields.push(entry);
    }
    let mut statement = connection.prepare(
        "SELECT i.instance_id,i.title,c.deployment_revision,c.instance_revision,c.connected_at \
        FROM auth_instances i LEFT JOIN auth_config_connections c USING(instance_id) \
        WHERE i.deployment_id=?1 AND i.state='active' AND (?2 IS NULL OR i.instance_id=?2) \
        ORDER BY i.instance_id",
    )?;
    let instances = statement
        .query_map(params![target.deployment(), target.instance()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u32>>(2)?,
                row.get::<_, Option<u32>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(
            |(instance, title, revision, instance_revision, connected)| {
                Ok(json!({
                    "instanceId": instance,
                    "title": title,
                    "connectedRevision": revision,
                    "connectedInstanceRevision": instance_revision,
                    "connectedAt": connected.map(timestamp).transpose()?,
                }))
            },
        )
        .collect::<Result<Vec<_>, AuthError>>()?;
    let revision = match target {
        Target::Deployment { .. } | Target::Client { .. } => {
            deployment.as_ref().map(|stored| stored.revision)
        }
        Target::Instance { .. } => instance.as_ref().map(|stored| stored.revision),
    };
    Ok(serde_json::from_value(json!({
        "target": target.wire()?,
        "revision": revision,
        "deploymentRevision": target
            .instance()
            .and(deployment.as_ref().map(|stored| stored.revision)),
        "fields": fields,
        "missing": missing,
        "instances": instances,
    }))?)
}

/// Record a new complete revision and tell the target's instances.
#[allow(clippy::too_many_arguments)]
fn write_revision(
    connection: &Connection,
    storage: &OAuthStorage,
    target: &Target,
    revision: u32,
    values: &Map<String, Value>,
    secrets: &Map<String, Value>,
    changed: &BTreeSet<String>,
    changed_secrets: &BTreeSet<String>,
    actor: &str,
    now: i64,
) -> Result<(), AuthError> {
    let key = target.key();
    connection.execute(
        "INSERT INTO auth_config_revisions(target_key,revision,deployment_id,instance_id,\
        client_id,values_json,sealed_secrets,changed_json,changed_secrets_json,created_by,\
        created_at) VALUES(?1,?2,?3,?4,?11,?5,?6,?7,?8,?9,?10)",
        params![
            key,
            revision,
            target.deployment(),
            target.instance(),
            json(values)?,
            seal_secrets(storage, &key, secrets)?,
            json(changed)?,
            json(changed_secrets)?,
            actor,
            now,
            target.client()
        ],
    )?;
    announce(connection, target, Some(revision), now)
}

/// Durably queue `Config.Changed` for a target; it commits with the change.
/// `revision` is the target's current one, absent when nothing is stored.
fn announce(
    connection: &Connection,
    target: &Target,
    revision: Option<u32>,
    now: i64,
) -> Result<(), AuthError> {
    let event: AuthConfigChanged = serde_json::from_value(json!({
        "eventId": super::sqlite::id(),
        "occurredAt": timestamp(now)?,
        "deploymentId": target.deployment(),
        "instanceId": target.instance(),
        "clientId": target.client(),
        "revision": revision,
    }))?;
    enqueue_effect(
        connection,
        "event",
        &json!({
            "subject": <events::ConfigChanged as EventDescriptor>::SUBJECT,
            "descriptor": <events::ConfigChanged as EventDescriptor>::descriptor_identity()
                .map_err(|_| AuthError::Unavailable)?,
            "event": event,
        }),
        None,
        now,
    )?;
    Ok(())
}

fn changed_keys(before: &Map<String, Value>, after: &Map<String, Value>) -> BTreeSet<String> {
    before
        .keys()
        .chain(after.keys())
        .filter(|name| before.get(*name) != after.get(*name))
        .cloned()
        .collect()
}

fn set_values(
    storage: &OAuthStorage,
    identity: &Mutation,
    input: &AuthConfigSetRequest,
) -> Result<AuthConfigResponse, AuthError> {
    let values = input.values.clone().unwrap_or_default();
    let secrets = input.secrets.clone().unwrap_or_default();
    let unset = input.unset.clone().unwrap_or_default();
    // Secrets never reach the audit record or the replay digest's preimage.
    let audit = json!({
        "requestDigest": identity.request_digest,
        "target": input.target,
        "values": values,
        "secrets": secrets.keys().collect::<Vec<_>>(),
        "unset": unset,
        "expectedRevision": input.expected_revision,
    });
    let safe = Mutation {
        request_digest: digest(&audit)?,
        ..identity.clone()
    };
    let now = crate::clock::unix_now();
    storage
        .authority
        .mutate(&safe, "rpc.config.set", &audit, now, |connection| {
            let target = Target::resolve(connection, &input.target)?;
            let declared = Declared::of_target(connection, &target)?;
            let mut touches_settings = !values.is_empty();
            let mut touches_secrets = !secrets.is_empty();
            for name in &unset {
                if declared.field(name)?.secret {
                    touches_secrets = true;
                } else {
                    touches_settings = true;
                }
            }
            if !touches_settings && !touches_secrets {
                return Err(AuthError::Invalid("there is nothing to change".into()));
            }
            if touches_settings {
                SqliteAuthorizationStore::require_administration(
                    connection,
                    &identity.actor,
                    Administration::ManageConfig,
                )?;
            }
            if touches_secrets {
                SqliteAuthorizationStore::require_administration(
                    connection,
                    &identity.actor,
                    Administration::ManageConfigSecrets,
                )?;
            }
            let key = target.key();
            let previous = current(connection, &key)?;
            if previous.as_ref().map(|stored| stored.revision)
                != input.expected_revision.as_ref().map(|revision| revision.0)
            {
                return Err(AuthError::Conflict);
            }
            let before_values = previous
                .as_ref()
                .map(|stored| stored.values.clone())
                .unwrap_or_default();
            let before_secrets = open_secrets(
                storage,
                &key,
                previous
                    .as_ref()
                    .and_then(|stored| stored.sealed.as_deref()),
            )?;
            let mut after_values = before_values.clone();
            let mut after_secrets = before_secrets.clone();
            for (name, value) in &values {
                if declared.field(name)?.secret {
                    return Err(AuthError::Invalid(format!(
                        "{name} is a secret; set it as a secret"
                    )));
                }
                declared.check(name, value)?;
                after_values.insert(name.clone(), value.clone());
            }
            for (name, value) in &secrets {
                if !declared.field(name)?.secret {
                    return Err(AuthError::Invalid(format!(
                        "{name} is not a secret; set it as a value"
                    )));
                }
                declared.check(name, value)?;
                after_secrets.insert(name.clone(), value.clone());
            }
            for name in &unset {
                after_values.remove(name.as_str());
                after_secrets.remove(name.as_str());
            }
            let changed = changed_keys(&before_values, &after_values);
            let changed_secrets = changed_keys(&before_secrets, &after_secrets);
            if !changed.is_empty() || !changed_secrets.is_empty() {
                let revision = previous.as_ref().map_or(1, |stored| stored.revision + 1);
                write_revision(
                    connection,
                    storage,
                    &target,
                    revision,
                    &after_values,
                    &after_secrets,
                    &changed,
                    &changed_secrets,
                    &identity.actor,
                    now,
                )?;
            }
            response(connection, storage, &target)
        })
}

fn roll_back(
    storage: &OAuthStorage,
    identity: &Mutation,
    input: &AuthConfigRollbackRequest,
) -> Result<AuthConfigResponse, AuthError> {
    let now = crate::clock::unix_now();
    storage
        .authority
        .mutate(identity, "rpc.config.rollback", input, now, |connection| {
            let target = Target::resolve(connection, &input.target)?;
            let key = target.key();
            let previous = current(connection, &key)?.ok_or(AuthError::NotFound)?;
            if previous.revision != input.expected_revision.0 {
                return Err(AuthError::Conflict);
            }
            let earlier =
                stored(connection, &key, Some(input.revision.0))?.ok_or(AuthError::NotFound)?;
            let before_secrets = open_secrets(storage, &key, previous.sealed.as_deref())?;
            let after_secrets = open_secrets(storage, &key, earlier.sealed.as_deref())?;
            let changed = changed_keys(&previous.values, &earlier.values);
            let changed_secrets = changed_keys(&before_secrets, &after_secrets);
            if !changed.is_empty() {
                SqliteAuthorizationStore::require_administration(
                    connection,
                    &identity.actor,
                    Administration::ManageConfig,
                )?;
            }
            if !changed_secrets.is_empty() {
                SqliteAuthorizationStore::require_administration(
                    connection,
                    &identity.actor,
                    Administration::ManageConfigSecrets,
                )?;
            }
            if changed.is_empty() && changed_secrets.is_empty() {
                return Err(AuthError::Invalid(format!(
                    "revision {} has the same values as the current one",
                    earlier.revision
                )));
            }
            write_revision(
                connection,
                storage,
                &target,
                previous.revision + 1,
                &earlier.values,
                &after_secrets,
                &changed,
                &changed_secrets,
                &identity.actor,
                now,
            )?;
            response(connection, storage, &target)
        })
}

fn revisions(
    connection: &Connection,
    target: &Target,
    input: &AuthConfigRevisionsListRequest,
) -> Result<AuthConfigRevisionsListResponse, AuthError> {
    let limit = input
        .page
        .as_ref()
        .and_then(|page| page.limit)
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    let limit = limit as usize;
    let before = input
        .page
        .as_ref()
        .and_then(|page| page.cursor.as_deref())
        .map(|cursor| {
            cursor
                .parse::<u32>()
                .map_err(|_| AuthError::Invalid("invalid cursor".into()))
        })
        .transpose()?;
    let key = target.key();
    let latest = current(connection, &key)?.map(|stored| stored.revision);
    let mut statement = connection.prepare(
        "SELECT revision,values_json,changed_json,changed_secrets_json,created_by,created_at \
        FROM auth_config_revisions WHERE target_key=?1 AND (?2 IS NULL OR revision<?2) \
        ORDER BY revision DESC LIMIT ?3",
    )?;
    let mut rows = statement
        .query_map(params![key, before, limit + 1], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (rows.len() > limit).then(|| rows[limit - 1].0.to_string());
    rows.truncate(limit);
    let items = rows
        .into_iter()
        .map(|(revision, values, changed, secrets, by, at)| {
            Ok(serde_json::from_value(json!({
                "revision": revision,
                "createdAt": timestamp(at)?,
                "createdBy": by,
                "values": decode::<Value>(&values)?,
                "changed": decode::<Value>(&changed)?,
                "changedSecrets": decode::<Value>(&secrets)?,
                "current": Some(revision) == latest,
            }))?)
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    Ok(CursorPage {
        items,
        page: CursorPageInfo { next_cursor },
    })
}

/// A service or device reads its own effective configuration, secrets
/// included. At connect, the revisions it loaded are recorded so restart-only
/// settings show which instances still run older values.
fn own_config(
    storage: &OAuthStorage,
    principal: &str,
    at_connect: bool,
) -> Result<AuthOwnConfigResponse, AuthError> {
    let now = crate::clock::unix_now();
    storage.authority.transaction(|connection| {
        type Instance = (String, String, String, String, u64);
        let (instance, deployment, participant, kind, revision): Instance =
            connection
                .query_row(
                    "SELECT i.instance_id,i.deployment_id,d.participant_id,d.participant_kind,\
                    i.installed_revision \
                    FROM auth_instances i JOIN auth_deployments d USING(deployment_id) \
                    JOIN auth_principals p ON p.principal_id=i.principal_id \
                    WHERE i.principal_id=?1 AND i.state='active' AND d.state='active' \
                    AND p.state='active'",
                    [principal],
                    |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                    },
                )
                .optional()?
                .ok_or(AuthError::Denied)?;
        // The fields of the build this instance runs, which may trail its deployment.
        let declared = Declared::of(connection, &participant, revision)?;
        let deployment_key = format!("deployment:{deployment}");
        let instance_key = format!("instance:{instance}");
        let deployment_stored = current(connection, &deployment_key)?;
        let instance_stored = current(connection, &instance_key)?;
        let empty = Map::new();
        let deployment_secrets = open_secrets(
            storage,
            &deployment_key,
            deployment_stored.as_ref().and_then(|stored| stored.sealed.as_deref()),
        )?;
        let instance_secrets = open_secrets(
            storage,
            &instance_key,
            instance_stored.as_ref().and_then(|stored| stored.sealed.as_deref()),
        )?;
        let effective = effective(
            &declared,
            &[
                (
                    instance_stored.as_ref().map_or(&empty, |stored| &stored.values),
                    &instance_secrets,
                    "instance",
                ),
                (
                    deployment_stored.as_ref().map_or(&empty, |stored| &stored.values),
                    &deployment_secrets,
                    "deployment",
                ),
            ],
        )?;
        let mut values = Map::new();
        let mut missing = Vec::new();
        for (name, value) in effective {
            match value.value {
                Some(value) => {
                    values.insert(name, value);
                }
                None => missing.push(name),
            }
        }
        let deployment_revision = deployment_stored.as_ref().map(|stored| stored.revision);
        let instance_revision = instance_stored.as_ref().map(|stored| stored.revision);
        if at_connect {
            connection.execute(
                "INSERT INTO auth_config_connections(instance_id,deployment_revision,\
                instance_revision,connected_at) VALUES(?1,?2,?3,?4) \
                ON CONFLICT(instance_id) DO UPDATE SET deployment_revision=excluded.deployment_revision,\
                instance_revision=excluded.instance_revision,connected_at=excluded.connected_at",
                params![instance, deployment_revision, instance_revision, now],
            )?;
        }
        let section = if kind == "device" { "devices" } else { "services" };
        Ok(serde_json::from_value(json!({
            "deploymentRevision": deployment_revision,
            "instanceRevision": instance_revision,
            "values": values,
            "missing": missing,
            "configUrl": storage.authority.settings.console_origin.as_ref().map(|origin| {
                format!("{origin}/console/admin/{section}/{deployment}#settings")
            }),
        }))?)
    })
}
