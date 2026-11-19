//! Configuration a participant declares and Trellis keeps.
//!
//! See `design/core/config-patterns.md`. A service or device declares typed
//! fields in IDL. Administrators store values per deployment, with optional
//! overrides per instance; every write is a complete new revision of that
//! target. Values are stored for any field name, declared or not: each build
//! reads the fields it declares, checked against its own types, and ignores
//! the rest, so values for a later build can be set while an earlier one runs.
//! Secret values are sealed with the installation's storage key and only the
//! participant itself reads them back.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use trellis_idl::{
    compile_evidence, AppKind, AppRequestDefinition, ConfigFieldDefinition, PackageGraph,
};
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
                        // Deleting a revision that holds secrets destroys
                        // them, which only a role that can set secrets may do.
                        let holds_secrets = stored(connection, &key, Some(revision))?
                            .is_some_and(|stored| stored.sealed.is_some());
                        if holds_secrets {
                            require(
                                connection,
                                &identity.actor,
                                Administration::ManageConfigSecrets,
                                "deleting a revision that holds secrets",
                            )?;
                        }
                        // A build reads an earlier revision's value when a
                        // newer one doesn't fit it, so deleting that revision
                        // would change what the build uses with no new
                        // revision or `Config.Changed`. Every build in use
                        // must read the same values without it.
                        let builds = builds_in_use(connection, &target)?;
                        let read = |connection: &Connection| {
                            builds
                                .iter()
                                .map(|(_, declared)| {
                                    Ok(layer(connection, storage, declared, &key, None, "")?
                                        .fields
                                        .into_iter()
                                        .map(|(name, (value, _))| (name, value))
                                        .collect::<Vec<_>>())
                                })
                                .collect::<Result<Vec<_>, AuthError>>()
                        };
                        let before = read(connection)?;
                        let deleted = connection.execute(
                            "DELETE FROM auth_config_revisions \
                            WHERE target_key=?1 AND revision=?2",
                            params![key, revision],
                        )?;
                        if deleted == 0 {
                            return Err(AuthError::NotFound);
                        }
                        let after = read(connection)?;
                        for (((build, _), before), after) in builds.iter().zip(&before).zip(&after)
                        {
                            if let Some(((name, _), _)) = before
                                .iter()
                                .zip(after)
                                .find(|(before, after)| before != after)
                            {
                                return Err(AuthError::Invalid(format!(
                                    "revision {revision} still gives {build} its value for {name}; \
                                    set a value for {name} that fits that build, then delete it"
                                )));
                            }
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
                        let declared = match app_request(&graph, &input.request_id)?.kind() {
                            AppKind::Browser => "browser",
                            AppKind::Native => "native",
                        };
                        // The declaration names the app the client signs in
                        // as, so its IDL kind must be the registration's kind.
                        let registered: String = connection.query_row(
                            "SELECT kind FROM auth_oauth_clients WHERE client_id=?1",
                            [input.client_id.to_string()],
                            |row| row.get(0),
                        )?;
                        if declared != registered {
                            return Err(AuthError::Invalid(format!(
                                "{} is declared as a {declared} app, but app {} is registered \
                                as a {registered} app",
                                input.request_id, input.client_id
                            )));
                        }
                        // Connected users of the app reread only when told, so a
                        // declaration that changes what they read is announced.
                        let read = |connection: &Connection| -> Result<Vec<_>, AuthError> {
                            let declared = Declared::of_target(connection, &target)?;
                            let layer = layer(
                                connection,
                                storage,
                                &declared,
                                &target.key(),
                                None,
                                "client",
                            )?;
                            Ok(effective(&declared, &[layer])
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
                            let revision =
                                current(connection, &target.key())?.map(|stored| stored.revision);
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
                let storage = engine.server.store();
                storage.authority.transaction(|connection| {
                    let declared = Declared::client(connection, &client)?;
                    let layer = layer(
                        connection,
                        storage,
                        &declared,
                        &format!("client:{client}"),
                        None,
                        "client",
                    )?;
                    let revision = layer.revision;
                    let values = effective(&declared, &[layer])
                        .into_iter()
                        .filter_map(|(name, value)| value.value.map(|value| (name, value)))
                        .collect::<Map<_, _>>();
                    Ok(serde_json::from_value(json!({
                        "revision": revision,
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

/// Every build that reads `target`'s values, each named for an administrator:
/// the declaration the target is shown against and, for a deployment or an
/// instance, the build each of its active instances runs.
fn builds_in_use(
    connection: &Connection,
    target: &Target,
) -> Result<Vec<(String, Declared)>, AuthError> {
    let mut builds = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(deployment) = target.deployment() {
        let installed: Option<(String, u64)> = connection
            .query_row(
                "SELECT d.participant_id,b.installed_revision \
                FROM auth_deployments d JOIN auth_deployment_bindings b USING(deployment_id) \
                WHERE d.deployment_id=?1",
                [deployment],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let running = connection
            .prepare(
                "SELECT DISTINCT d.participant_id,i.installed_revision \
                FROM auth_instances i JOIN auth_deployments d USING(deployment_id) \
                WHERE i.deployment_id=?1 AND i.state='active' \
                AND (?2 IS NULL OR i.instance_id=?2)",
            )?
            .query_map(params![deployment, target.instance()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (participant, revision) in installed.into_iter().chain(running) {
            if seen.insert((participant.clone(), revision)) {
                builds.push((
                    format!("{participant} build {revision}"),
                    Declared::of(connection, &participant, revision)?,
                ));
            }
        }
    } else {
        let label = if target.client().is_some() {
            "the app's declaration"
        } else {
            "the build the service submitted"
        };
        builds.push((label.to_owned(), Declared::of_target(connection, target)?));
    }
    Ok(builds)
}

/// Like [`SqliteAuthorizationStore::require_administration`], but a denial
/// names the capability `action` needs.
fn require(
    connection: &Connection,
    actor: &str,
    administration: Administration,
    action: &str,
) -> Result<(), AuthError> {
    SqliteAuthorizationStore::require_administration(connection, actor, administration).map_err(
        |error| match error {
            AuthError::Denied => AuthError::DeniedNeeding(format!(
                "{action} needs {}",
                administration.capability_id()
            )),
            other => other,
        },
    )
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
    /// A service added without its IDL, until its service connects and an
    /// administrator approves it.
    Claim {
        claim: String,
    },
}

impl Target {
    fn resolve(connection: &Connection, target: &AuthConfigTarget) -> Result<Self, AuthError> {
        if let Some(claim) = &target.claim_id {
            if target.deployment_id.is_some()
                || target.instance_id.is_some()
                || target.client_id.is_some()
            {
                return Err(AuthError::Invalid(
                    "name a service claim alone, without a deployment or client".into(),
                ));
            }
            let claim = claim.to_string();
            let found: Option<(String, Option<String>)> = connection
                .query_row(
                    "SELECT state,deployment_id FROM auth_service_claims WHERE claim_id=?1",
                    [&claim],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            // An approved claim is its deployment now, settings included.
            return match found {
                Some((state, _)) if state == "waiting" => Ok(Self::Claim { claim }),
                Some((state, Some(deployment))) if state == "claimed" => {
                    Ok(Self::Deployment { deployment })
                }
                _ => Err(AuthError::NotFound),
            };
        }
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
            Self::Claim { claim } => format!("claim:{claim}"),
        }
    }

    /// The key whose values sit under any instance overrides.
    fn base_key(&self) -> String {
        match self {
            Self::Deployment { deployment } | Self::Instance { deployment, .. } => {
                format!("deployment:{deployment}")
            }
            Self::Client { .. } | Self::Claim { .. } => self.key(),
        }
    }

    fn deployment(&self) -> Option<&str> {
        match self {
            Self::Deployment { deployment } | Self::Instance { deployment, .. } => Some(deployment),
            Self::Client { .. } | Self::Claim { .. } => None,
        }
    }

    fn instance(&self) -> Option<&str> {
        match self {
            Self::Instance { instance, .. } => Some(instance),
            _ => None,
        }
    }

    fn client(&self) -> Option<&str> {
        match self {
            Self::Client { client } => Some(client),
            _ => None,
        }
    }

    fn claim(&self) -> Option<&str> {
        match self {
            Self::Claim { claim } => Some(claim),
            _ => None,
        }
    }

    fn wire(&self) -> Result<AuthConfigTarget, AuthError> {
        Ok(serde_json::from_value(json!({
            "deploymentId": self.deployment(),
            "instanceId": self.instance(),
            "clientId": self.client(),
            "claimId": self.claim(),
        }))?)
    }
}

/// The fields one build declares, with the graph their types live in.
struct Declared {
    graph: Option<Arc<PackageGraph>>,
    fields: BTreeMap<String, ConfigFieldDefinition>,
}

impl Declared {
    fn none() -> Self {
        Self {
            graph: None,
            fields: BTreeMap::new(),
        }
    }

    /// The fields `participant` declares in a package evidence document.
    fn compile(evidence: &str, participant: &str) -> Result<Self, AuthError> {
        let graph = compile_evidence(decode(evidence)?)
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

    fn of(connection: &Connection, participant: &str, revision: u64) -> Result<Self, AuthError> {
        let evidence: String = connection.query_row(
            "SELECT projection_json FROM auth_installed_participants \
            WHERE participant_id=?1 AND revision=?2",
            params![participant, revision],
            |row| row.get(0),
        )?;
        Self::compile(&evidence, participant)
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
            None => Ok(Self::none()),
        }
    }

    /// The fields of the build a claim's service submitted, once it connected.
    fn submitted(connection: &Connection, claim: &str) -> Result<Self, AuthError> {
        let pending: Option<(String, String)> = connection
            .query_row(
                "SELECT participant_id,evidence_json FROM auth_service_updates \
                WHERE deployment_id=?1 AND state='pending' \
                ORDER BY requested_at DESC,update_id DESC LIMIT 1",
                [claim],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match pending {
            Some((participant, evidence)) => Self::compile(&evidence, &participant),
            None => Ok(Self::none()),
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
            return Ok(Self::none());
        };
        let graph = compile_evidence(decode(&evidence)?)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let fields = app_request(&graph, &request)?.config().clone();
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
            Target::Claim { claim } => Self::submitted(connection, claim),
        }
    }

    /// Why `value`, stored for the declared field `name`, doesn't fit its type.
    /// A secret's value never appears in the reason.
    fn problem(&self, name: &str, value: &Value) -> Result<Option<String>, AuthError> {
        let field = &self.fields[name];
        let graph = self.graph.as_deref().ok_or(AuthError::Unavailable)?;
        let schema = schema(graph, field)?;
        let validator = jsonschema::validator_for(&schema)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        let problem = validator.iter_errors(value).next().map(|error| {
            if field.secret {
                format!("{name}: the stored secret doesn't fit the declared type")
            } else {
                format!("{name}: {error}")
            }
        });
        Ok(problem)
    }
}

fn schema(graph: &PackageGraph, field: &ConfigFieldDefinition) -> Result<Value, AuthError> {
    trellis_idl::expression_json_schema(graph, &field.ty)
        .map_err(|error| AuthError::Invalid(error.to_string()))
}

/// A declared field as `AuthConfigDeclaration` describes it.
pub(super) fn declaration(
    graph: &PackageGraph,
    name: &str,
    field: &ConfigFieldDefinition,
) -> Result<Map<String, Value>, AuthError> {
    let mut entry = json!({
        "name": name,
        "title": field.docs.title,
        "description": field.docs.description,
        "schema": schema(graph, field)?,
        "required": field.default.is_none(),
        "secret": field.secret,
        "restart": field.restart,
    });
    // Absent and JSON null differ for `json` fields, so absent keys stay absent.
    if let Some(default) = &field.default {
        entry["default"] = default.clone();
    }
    let Value::Object(entry) = entry else {
        unreachable!("a JSON object literal");
    };
    Ok(entry)
}

/// One app request `graph` declares.
fn app_request<'a>(
    graph: &'a PackageGraph,
    request: &str,
) -> Result<&'a AppRequestDefinition, AuthError> {
    graph
        .packages()
        .values()
        .flat_map(|package| package.app_requests().values())
        .find(|definition| definition.identity().as_str() == request)
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

/// What one target's stored revisions give a build.
struct Layer {
    source: &'static str,
    /// The revision read, absent when nothing is stored.
    revision: Option<u32>,
    /// For each declared field, the value this build uses from here, and why
    /// the newest value stored here doesn't fit it.
    fields: BTreeMap<String, (Option<Value>, Option<String>)>,
    /// The revision's settings and secrets, for what the build doesn't declare.
    values: Map<String, Value>,
    secrets: Map<String, Value>,
}

/// Read `key` as of revision `upto` (the current one when absent) for the
/// build that declares `declared`. A stored value that doesn't fit the build,
/// by type or by being a secret where the build wants a setting or the other
/// way round, is skipped for the value the field had before it, so a value
/// set for a later build never disturbs the one running. A field stored
/// nowhere before that has no value here.
fn layer(
    connection: &Connection,
    storage: &OAuthStorage,
    declared: &Declared,
    key: &str,
    upto: Option<u32>,
    source: &'static str,
) -> Result<Layer, AuthError> {
    let mut statement = connection.prepare(
        "SELECT revision,values_json,sealed_secrets FROM auth_config_revisions \
        WHERE target_key=?1 AND (?2 IS NULL OR revision<=?2) ORDER BY revision DESC",
    )?;
    let rows = statement.query_map(params![key, upto], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    })?;
    let mut result = Layer {
        source,
        revision: None,
        fields: BTreeMap::new(),
        values: Map::new(),
        secrets: Map::new(),
    };
    let mut problems = BTreeMap::new();
    let mut open: Vec<&String> = declared.fields.keys().collect();
    for row in rows {
        if open.is_empty() && result.revision.is_some() {
            break;
        }
        let (revision, values, sealed) = row?;
        let values: Map<String, Value> = decode(&values)?;
        let secrets = open_secrets(storage, key, sealed.as_deref())?;
        let newest = result.revision.is_none();
        let mut unresolved = Vec::new();
        for name in open {
            let field = &declared.fields[name];
            let (wanted, other) = if field.secret {
                (&secrets, &values)
            } else {
                (&values, &secrets)
            };
            let problem = match wanted.get(name.as_str()) {
                Some(value) => match declared.problem(name, value)? {
                    None => {
                        let problem = problems.remove(name);
                        result
                            .fields
                            .insert(name.clone(), (Some(value.clone()), problem));
                        continue;
                    }
                    Some(problem) => problem,
                },
                None if other.contains_key(name.as_str()) => {
                    if field.secret {
                        format!(
                            "{name} is stored as a plain setting, but this build declares it \
                            secret; set it as a secret"
                        )
                    } else {
                        format!(
                            "{name} is stored as a secret, but this build declares a plain \
                            setting; set it as a value"
                        )
                    }
                }
                None => {
                    let problem = problems.remove(name);
                    result.fields.insert(name.clone(), (None, problem));
                    continue;
                }
            };
            if newest {
                problems.insert(name.clone(), problem);
            }
            unresolved.push(name);
        }
        open = unresolved;
        if newest {
            result.revision = Some(revision);
            result.values = values;
            result.secrets = secrets;
        }
    }
    for name in open {
        let problem = problems.remove(name);
        result.fields.insert(name.clone(), (None, problem));
    }
    Ok(result)
}

/// A field's value after instance overrides, deployment values, and defaults.
struct Effective {
    value: Option<Value>,
    source: &'static str,
    invalid: Option<String>,
}

/// Layers are most specific first.
fn effective(declared: &Declared, layers: &[Layer]) -> BTreeMap<String, Effective> {
    declared
        .fields
        .iter()
        .map(|(name, field)| {
            let mut found = None;
            let mut invalid = None;
            for layer in layers {
                let (value, problem) = &layer.fields[name];
                invalid = invalid.or_else(|| problem.clone());
                if let Some(value) = value {
                    found = Some((value.clone(), layer.source));
                    break;
                }
            }
            let (value, source) = match (found, &field.default) {
                (Some((value, source)), _) => (Some(value), source),
                (None, Some(default)) => (Some(default.clone()), "default"),
                (None, None) => (None, "unset"),
            };
            (
                name.clone(),
                Effective {
                    value,
                    source,
                    invalid,
                },
            )
        })
        .collect()
}

/// The target's own layer and, for an instance, its deployment's beneath it,
/// each as of `upto` (current when absent). A layer an instance had nothing
/// stored in when it connected is left out.
fn layers(
    connection: &Connection,
    storage: &OAuthStorage,
    declared: &Declared,
    target: &Target,
    upto: Option<(Option<u32>, Option<u32>)>,
) -> Result<Vec<Layer>, AuthError> {
    let base = match target {
        Target::Client { .. } => "client",
        _ => "deployment",
    };
    let mut layers = Vec::new();
    match upto {
        None => {
            if target.instance().is_some() {
                layers.push(layer(
                    connection,
                    storage,
                    declared,
                    &target.key(),
                    None,
                    "instance",
                )?);
            }
            layers.push(layer(
                connection,
                storage,
                declared,
                &target.base_key(),
                None,
                base,
            )?);
        }
        Some((instance, deployment)) => {
            if let (Some(revision), Some(_)) = (instance, target.instance()) {
                layers.push(layer(
                    connection,
                    storage,
                    declared,
                    &target.key(),
                    Some(revision),
                    "instance",
                )?);
            }
            if let Some(revision) = deployment {
                layers.push(layer(
                    connection,
                    storage,
                    declared,
                    &target.base_key(),
                    Some(revision),
                    base,
                )?);
            }
        }
    }
    Ok(layers)
}

/// The administrator's view of a target: fields with settings shown and
/// secrets reduced to whether they are set, stored values the build doesn't
/// use, plus each instance's connect state.
fn response(
    connection: &Connection,
    storage: &OAuthStorage,
    target: &Target,
) -> Result<AuthConfigResponse, AuthError> {
    let declared = Declared::of_target(connection, target)?;
    let layers = layers(connection, storage, &declared, target, None)?;
    let resolved = effective(&declared, &layers);
    let mut fields = Vec::new();
    let mut missing = Vec::new();
    if let Some(graph) = declared.graph.as_deref() {
        for (name, field) in &declared.fields {
            let value = &resolved[name];
            if value.value.is_none() {
                missing.push(name.clone());
            }
            let mut entry = declaration(graph, name, field)?;
            entry.insert(
                "isSet".into(),
                json!(matches!(value.source, "instance" | "deployment" | "client")),
            );
            entry.insert("source".into(), json!(value.source));
            // A plain value saved here before the build declared the field
            // secret: never shown, only reported, so it can be set as one.
            entry.insert(
                "storedAsPlain".into(),
                json!(field.secret && layers[0].values.contains_key(name.as_str())),
            );
            if let (false, Some(current)) = (field.secret, &value.value) {
                entry.insert("value".into(), current.clone());
            }
            if let Some(invalid) = &value.invalid {
                entry.insert("invalid".into(), json!(invalid));
            }
            fields.push(Value::Object(entry));
        }
    }
    let mut unused = Vec::new();
    let mut seen = BTreeSet::new();
    for layer in &layers {
        for (name, value) in &layer.values {
            if !declared.fields.contains_key(name) && seen.insert(name.clone()) {
                unused.push(json!({
                    "name": name,
                    "secret": false,
                    "value": value,
                    "source": layer.source,
                }));
            }
        }
        for name in layer.secrets.keys() {
            if !declared.fields.contains_key(name) && seen.insert(name.clone()) {
                unused.push(json!({ "name": name, "secret": true, "source": layer.source }));
            }
        }
    }
    let mut statement = connection.prepare(
        "SELECT i.instance_id,i.title,c.deployment_revision,c.instance_revision,c.connected_at,\
        d.participant_id,i.installed_revision \
        FROM auth_instances i JOIN auth_deployments d ON d.deployment_id=i.deployment_id \
        LEFT JOIN auth_config_connections c ON c.instance_id=i.instance_id \
        WHERE i.deployment_id=?1 AND i.state='active' AND (?2 IS NULL OR i.instance_id=?2) \
        ORDER BY i.instance_id",
    )?;
    type Row = (
        String,
        String,
        Option<u32>,
        Option<u32>,
        Option<i64>,
        String,
        u64,
    );
    let rows = statement
        .query_map(params![target.deployment(), target.instance()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<Row>>>()?;
    let mut builds: BTreeMap<u64, Declared> = BTreeMap::new();
    let mut instances = Vec::new();
    for (instance, title, revision, instance_revision, connected, participant, build) in rows {
        // Only what the instance's own build reads after a restart counts:
        // values for fields it doesn't declare, or that don't fit it, leave
        // it as it is.
        let restart_pending = match connected {
            None => false,
            Some(_) => {
                let declared = match builds.entry(build) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(Declared::of(connection, &participant, build)?)
                    }
                };
                let declared = &*declared;
                let own = Target::Instance {
                    deployment: target.deployment().unwrap_or_default().to_owned(),
                    instance: instance.clone(),
                };
                let now = effective(
                    declared,
                    &self::layers(connection, storage, declared, &own, None)?,
                );
                let then = effective(
                    declared,
                    &self::layers(
                        connection,
                        storage,
                        declared,
                        &own,
                        Some((instance_revision, revision)),
                    )?,
                );
                declared
                    .fields
                    .iter()
                    .any(|(name, field)| field.restart && now[name].value != then[name].value)
            }
        };
        instances.push(json!({
            "instanceId": instance,
            "title": title,
            "connectedRevision": revision,
            "connectedInstanceRevision": instance_revision,
            "connectedAt": connected.map(timestamp).transpose()?,
            "restartPending": restart_pending,
        }));
    }
    let deployment_revision = match target {
        Target::Instance { .. } => layers.get(1).and_then(|layer| layer.revision),
        _ => None,
    };
    Ok(serde_json::from_value(json!({
        "target": target.wire()?,
        "revision": layers[0].revision,
        "deploymentRevision": deployment_revision,
        "declared": declared.graph.is_some(),
        "fields": fields,
        "missing": missing,
        "unused": unused,
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
        client_id,claim_id,values_json,sealed_secrets,changed_json,changed_secrets_json,\
        created_by,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            key,
            revision,
            target.deployment(),
            target.instance(),
            target.client(),
            target.claim(),
            json(values)?,
            seal_secrets(storage, &key, secrets)?,
            json(changed)?,
            json(changed_secrets)?,
            actor,
            now,
        ],
    )?;
    // Nothing runs as a claim yet: its service reads the settings when it
    // connects as the deployment's instance.
    if target.claim().is_some() {
        return Ok(());
    }
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

/// Store values for any field name, declared or not. Whether a value fits is
/// each build's own question, answered when it reads them, so a value meant
/// for a later build can be set while an earlier one runs.
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
            for name in values.keys().chain(secrets.keys()) {
                // The IDL's rule for field names, so a build can declare it.
                let mut characters = name.chars();
                if !characters.next().is_some_and(|c| c.is_ascii_lowercase())
                    || !characters.all(|c| c.is_ascii_alphanumeric())
                {
                    return Err(AuthError::Invalid(format!(
                        "{name} isn't a setting name; names are lowerCamelCase, such as databaseUrl"
                    )));
                }
                if values.contains_key(name) && secrets.contains_key(name) {
                    return Err(AuthError::Invalid(format!(
                        "{name} is given both as a setting and as a secret"
                    )));
                }
                // A value typed for a known secret would be stored where
                // every administrator reads it.
                if values.contains_key(name)
                    && declared.fields.get(name).is_some_and(|field| field.secret)
                {
                    return Err(AuthError::Invalid(format!(
                        "{name} is a secret; set it as a secret"
                    )));
                }
            }
            // Every user of an app reads its settings, so nothing is secret.
            if target.client().is_some() && !secrets.is_empty() {
                return Err(AuthError::Invalid(
                    "an app's settings are public, so it has no secrets".into(),
                ));
            }
            if values.is_empty() && secrets.is_empty() && unset.is_empty() {
                return Err(AuthError::Invalid("there is nothing to change".into()));
            }
            let key = target.key();
            let previous = current(connection, &key)?;
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
            let mut touches_settings = !values.is_empty();
            let mut touches_secrets = !secrets.is_empty();
            for name in &unset {
                let secret = match declared.fields.get(name.as_str()) {
                    Some(field) => field.secret,
                    None => before_secrets.contains_key(name.as_str()),
                };
                if secret {
                    touches_secrets = true;
                } else {
                    touches_settings = true;
                }
            }
            // A field is stored either as a setting or as a secret.
            let mut after_values = before_values.clone();
            let mut after_secrets = before_secrets.clone();
            for (name, value) in &values {
                after_secrets.remove(name);
                after_values.insert(name.clone(), value.clone());
            }
            for (name, value) in &secrets {
                after_values.remove(name);
                after_secrets.insert(name.clone(), value.clone());
            }
            for name in &unset {
                after_values.remove(name.as_str());
                after_secrets.remove(name.as_str());
            }
            let changed = changed_keys(&before_values, &after_values);
            let changed_secrets = changed_keys(&before_secrets, &after_secrets);
            // A plain value under a name the build declares secret goes when
            // the secret is set, which is the secret's business alone.
            let changes_settings = changed
                .iter()
                .any(|name| !declared.fields.get(name).is_some_and(|field| field.secret));
            if touches_settings || changes_settings {
                require(
                    connection,
                    &identity.actor,
                    Administration::ManageConfig,
                    "changing settings",
                )?;
            }
            if touches_secrets || !changed_secrets.is_empty() {
                require(
                    connection,
                    &identity.actor,
                    Administration::ManageConfigSecrets,
                    "setting secrets",
                )?;
            }
            if previous.as_ref().map(|stored| stored.revision)
                != input.expected_revision.as_ref().map(|revision| revision.0)
            {
                return Err(AuthError::Conflict);
            }
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
                require(
                    connection,
                    &identity.actor,
                    Administration::ManageConfig,
                    "rolling back settings",
                )?;
            }
            if !changed_secrets.is_empty() {
                require(
                    connection,
                    &identity.actor,
                    Administration::ManageConfigSecrets,
                    "rolling back secrets",
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
    // A plain value saved before the build declared its field secret is
    // never shown again once it is.
    let declared = Declared::of_target(connection, target)?;
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
            let mut values = decode::<Map<String, Value>>(&values)?;
            let hidden = declared
                .fields
                .iter()
                .filter(|(name, field)| field.secret && values.remove(name.as_str()).is_some())
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            Ok(serde_json::from_value(json!({
                "revision": revision,
                "createdAt": timestamp(at)?,
                "createdBy": by,
                "values": values,
                "hiddenValues": hidden,
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

/// Approving a claim makes its settings the new deployment's, every revision
/// included. Secrets are sealed to their target, so each revision's secrets
/// are sealed again for the deployment.
pub(super) fn adopt_claim(
    connection: &Connection,
    storage: &OAuthStorage,
    claim: &str,
    deployment: &str,
) -> Result<(), AuthError> {
    let from = format!("claim:{claim}");
    let to = format!("deployment:{deployment}");
    let rows = connection
        .prepare("SELECT revision,sealed_secrets FROM auth_config_revisions WHERE target_key=?1")?
        .query_map([&from], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (revision, sealed) in rows {
        let secrets = open_secrets(storage, &from, sealed.as_deref())?;
        connection.execute(
            "UPDATE auth_config_revisions SET target_key=?3,deployment_id=?4,claim_id=NULL,\
            sealed_secrets=?5 WHERE target_key=?1 AND revision=?2",
            params![
                from,
                revision,
                to,
                deployment,
                seal_secrets(storage, &to, &secrets)?
            ],
        )?;
    }
    Ok(())
}

/// A service or device reads its own effective configuration, secrets
/// included, as the build it runs declares it. At connect, the revisions it
/// loaded are recorded so restart-only settings show which instances still
/// run older values.
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
        let target = Target::Instance {
            deployment: deployment.clone(),
            instance: instance.clone(),
        };
        let layers = layers(connection, storage, &declared, &target, None)?;
        let instance_revision = layers[0].revision;
        let deployment_revision = layers[1].revision;
        let mut values = Map::new();
        let mut missing = Vec::new();
        let mut invalid = Map::new();
        for (name, value) in effective(&declared, &layers) {
            if let Some(problem) = value.invalid {
                invalid.insert(name.clone(), json!(problem));
            }
            match value.value {
                Some(value) => {
                    values.insert(name, value);
                }
                None => missing.push(name),
            }
        }
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
            "invalid": invalid,
            "configUrl": storage.authority.settings.console_origin.as_ref().map(|origin| {
                format!("{origin}/console/admin/{section}/{deployment}#settings")
            }),
        }))?)
    })
}
