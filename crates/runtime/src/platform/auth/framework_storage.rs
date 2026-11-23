//! Operational-owner storage for the existing deployment-scoped Operation runtime.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_protocol::ApiSurfaceKind;

use super::{
    authorization_sessions::ProviderImplementation,
    sqlite::{decode, digest, json, AuthError, SqliteAuthorizationStore},
};

/// Committed framework storage; these are not participant-declared resources.
#[derive(Deserialize, Serialize)]
pub(super) struct OperationStorage {
    pub(super) repository: String,
    pub(super) staging: String,
}

impl OperationStorage {
    /// The buckets Trellis makes for `deployment`'s Operations.
    fn of(deployment: &str) -> Self {
        Self {
            repository: format!("trellis_operations_{deployment}"),
            staging: format!("trellis_operation_staging_{deployment}"),
        }
    }
}

/// The description that marks a bucket as the one Trellis made.
fn marker(deployment: &str, object_store: bool) -> String {
    format!(
        "trellis.framework.operation.v1:{deployment}:{}",
        if object_store {
            "staging"
        } else {
            "repository"
        }
    )
}

/// Marks a deployment's Operation storage as being prepared until dropped.
///
/// Preparation checks that the deployment is active, then creates its
/// buckets, then checks again. A removal can commit in between, and its
/// deletion could then find no buckets and finish before they are created, so
/// [`delete_operations`] waits while this is held. Removal is final, so a
/// preparation that starts after the removal commits creates nothing. A server
/// that stops mid-preparation holds nothing after it starts again, and the
/// removal's deletion, retried until done, finds what was created.
struct Preparing<'a> {
    store: &'a SqliteAuthorizationStore,
    deployment: &'a str,
}

impl<'a> Preparing<'a> {
    fn start(store: &'a SqliteAuthorizationStore, deployment: &'a str) -> Self {
        *store
            .preparing_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(deployment.to_owned())
            .or_default() += 1;
        Self { store, deployment }
    }
}

impl Drop for Preparing<'_> {
    fn drop(&mut self) {
        let mut preparing = self
            .store
            .preparing_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = preparing.get_mut(self.deployment) {
            *count -= 1;
            if *count == 0 {
                preparing.remove(self.deployment);
            }
        }
    }
}

/// Delete the Operation storage Trellis made for a removed deployment: its
/// Operation records and staged uploads. A bucket already gone counts as
/// deleted; one Trellis did not make is left in place. While the deployment's
/// storage is still being prepared this fails, to be retried, so a bucket
/// created after the removal is not left behind.
pub(super) async fn delete_operations(
    store: &SqliteAuthorizationStore,
    operational: &async_nats::Client,
    deployment: &str,
) -> Result<(), AuthError> {
    use async_nats::jetstream::context::GetStreamErrorKind;
    if store
        .preparing_operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(deployment)
    {
        return Err(AuthError::Unavailable);
    }
    let jetstream = async_nats::jetstream::new(operational.clone());
    let buckets = OperationStorage::of(deployment);
    for (bucket, object_store) in [(&buckets.repository, false), (&buckets.staging, true)] {
        let stream_name = format!("{}_{bucket}", if object_store { "OBJ" } else { "KV" });
        let mut stream = match jetstream.get_stream(&stream_name).await {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    GetStreamErrorKind::JetStream(error)
                        if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND
                ) =>
            {
                continue;
            }
            Err(error) => return Err(AuthError::Broker(error.to_string())),
        };
        let info = stream
            .info()
            .await
            .map_err(|error| AuthError::Broker(error.to_string()))?;
        if info.config.description.as_deref() != Some(marker(deployment, object_store).as_str()) {
            return Err(AuthError::Invalid(format!(
                "stream {stream_name} is not the one Trellis made for deployment {deployment}, so \
                Trellis leaves it in place"
            )));
        }
        jetstream
            .delete_stream(&stream_name)
            .await
            .map_err(|error| AuthError::Broker(error.to_string()))?;
    }
    Ok(())
}

/// Provision existing Operation storage before admitting an implemented provider.
pub(super) async fn materialize_operations(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    deployment: &str,
    now: i64,
) -> Result<(), AuthError> {
    // Held from before the first check until the last.
    let _preparing = Preparing::start(store, deployment);
    let selected = deployment.to_owned();
    let plan = store
        .transaction_blocking(move |connection| {
            let deployment = selected.as_str();
            let (participant, installed_revision, binding_revision, implementations): (
                String,
                u64,
                u64,
                String,
            ) = connection.query_row(
                "SELECT d.participant_id,b.installed_revision,b.revision,\
            b.provided_apis_json \
            FROM auth_deployment_bindings b \
            JOIN auth_deployments d USING(deployment_id) \
            WHERE d.deployment_id=?1 \
            AND d.state='active' \
            AND EXISTS(SELECT 1 \
            FROM auth_instances i \
            JOIN auth_principals p ON p.principal_id=i.principal_id \
            WHERE i.deployment_id=b.deployment_id \
            AND i.state='active' \
            AND p.state='active')",
                [deployment],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            let implementations: Vec<ProviderImplementation> = decode(&implementations)?;
            if !implementations.iter().any(|implementation| {
                implementation
                    .implemented_actions
                    .iter()
                    .any(|action| action.kind == ApiSurfaceKind::Operation)
            }) {
                return Ok(None);
            }
            // An admission must not use an earlier preparation while this startup
            // owner is checking durable, externally persisted storage ownership.
            let materialized: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
            FROM auth_resource_binding_evidence \
            WHERE owner_kind='deployment' \
            AND owner_id=?1 AND resource_kind='framework_operation')",
                [deployment],
                |row| row.get(0),
            )?;
            connection.execute(
                "UPDATE auth_resource_binding_evidence \
            SET state='unavailable' \
            WHERE owner_kind='deployment' \
            AND owner_id=?1 AND participant_id=?2 \
            AND resource_kind='framework_operation'",
                params![deployment, participant],
            )?;
            Ok(Some((
                participant,
                installed_revision,
                binding_revision,
                materialized,
            )))
        })
        .await?;
    let Some((participant, installed_revision, binding_revision, materialized)) = plan else {
        return Ok(());
    };
    let bindings = OperationStorage::of(deployment);
    let jetstream = async_nats::jetstream::new(operational.clone());
    for (bucket, object_store) in [(&bindings.repository, false), (&bindings.staging, true)] {
        let marker = marker(deployment, object_store);
        let stream_name = format!("{}_{bucket}", if object_store { "OBJ" } else { "KV" });
        match jetstream.get_stream(&stream_name).await {
            Ok(_) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    async_nats::jetstream::context::GetStreamErrorKind::JetStream(error)
                        if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND
                ) =>
            {
                if materialized {
                    return Err(AuthError::Broker(format!(
                        "previously materialized Operation storage '{bucket}' is missing"
                    )));
                }
                if object_store {
                    jetstream
                        .create_object_store(async_nats::jetstream::object_store::Config {
                            bucket: bucket.clone(),
                            description: Some(marker.clone()),
                            storage: async_nats::jetstream::stream::StorageType::File,
                            ..Default::default()
                        })
                        .await
                        .map_err(|error| AuthError::Broker(error.to_string()))?;
                } else {
                    jetstream
                        .create_key_value(async_nats::jetstream::kv::Config {
                            bucket: bucket.clone(),
                            description: marker.clone(),
                            history: 1,
                            storage: async_nats::jetstream::stream::StorageType::File,
                            ..Default::default()
                        })
                        .await
                        .map_err(|error| AuthError::Broker(error.to_string()))?;
                }
            }
            Err(error) => return Err(AuthError::Broker(error.to_string())),
        }
        let mut stream = jetstream
            .get_stream(&stream_name)
            .await
            .map_err(|error| AuthError::Broker(error.to_string()))?;
        let config = &stream
            .info()
            .await
            .map_err(|error| AuthError::Broker(error.to_string()))?
            .config;
        // Adopting another owner's store leaks durable workflows; memory storage
        // violates their ordinary restart durability. Never repair either by
        // overwriting or silently recreating an existing stream.
        let subjects = if object_store {
            vec![format!("$O.{bucket}.C.>"), format!("$O.{bucket}.M.>")]
        } else {
            vec![format!("$KV.{bucket}.>")]
        };
        if config.description.as_deref() != Some(marker.as_str())
            || config.storage != async_nats::jetstream::stream::StorageType::File
            || config
                .subjects
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                != subjects.iter().collect()
        {
            return Err(AuthError::Invalid(format!(
                "stream {} is not the one Trellis made for this deployment, so Trellis leaves it \
                in place",
                config.name
            )));
        }
    }
    let selected = deployment.to_owned();
    store
        .transaction_blocking(move |connection| {
            let deployment = selected.as_str();
            let current: Option<(u64, u64)> = connection
                .query_row(
                    "SELECT b.installed_revision,b.revision \
                FROM auth_deployment_bindings b \
                JOIN auth_deployments d USING(deployment_id) \
                WHERE b.deployment_id=?1 \
                AND d.state='active'",
                    [deployment],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if current != Some((installed_revision, binding_revision)) {
                return Err(AuthError::Conflict(format!(
                    "deployment {deployment} changed while Trellis was preparing its storage; try \
                    again"
                )));
            }
            connection.execute(
                "INSERT INTO auth_resource_binding_evidence(owner_kind,owner_id,\
            participant_id,installed_revision,resource_kind,local_name,binding_id,\
            provider_identity,actual_json,state,materialized_at) \
            VALUES('deployment',?1,?2,?3,'framework_operation','operations',?4,?1,\
            ?5,'available',?6) \
            ON CONFLICT(owner_kind,owner_id,participant_id,installed_revision,\
            resource_kind,local_name) \
            DO UPDATE SET actual_json=excluded.actual_json,state='available',\
            materialized_at=excluded.materialized_at",
                params![
                    deployment,
                    participant,
                    installed_revision,
                    digest(&("framework_operation", deployment))?,
                    json(&bindings)?,
                    now
                ],
            )?;
            Ok(())
        })
        .await
}
