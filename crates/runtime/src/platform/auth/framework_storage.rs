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

/// Provision existing Operation storage before admitting an implemented provider.
pub(super) async fn materialize_operations(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    deployment: &str,
    now: i64,
) -> Result<(), AuthError> {
    let plan = store.transaction(|connection| {
        let (participant, installed_revision, binding_revision, implementations): (String,u64,u64,String) = connection.query_row(
            "SELECT d.participant_id,b.installed_revision,b.revision,b.provided_apis_json FROM auth_deployment_bindings b JOIN auth_deployments d USING(deployment_id) WHERE d.deployment_id=?1 AND d.state='active' AND EXISTS(SELECT 1 FROM auth_instances i JOIN auth_principals p ON p.principal_id=i.principal_id WHERE i.deployment_id=b.deployment_id AND i.installed_revision=b.installed_revision AND i.state='active' AND p.state='active')",
            [deployment], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        let implementations: Vec<ProviderImplementation> = decode(&implementations)?;
        if !implementations.iter().any(|implementation| implementation.implemented_actions.iter().any(|action| action.kind == ApiSurfaceKind::Operation)) { return Ok(None); }
        // An admission must not use an earlier preparation while this startup
        // owner is checking durable, externally persisted storage ownership.
        connection.execute("UPDATE auth_resource_binding_evidence SET state='unavailable' WHERE owner_kind='deployment' AND owner_id=?1 AND participant_id=?2 AND installed_revision=?3 AND resource_kind='framework_operation'",params![deployment,participant,installed_revision])?;
        Ok(Some((participant,installed_revision,binding_revision)))
    })?;
    let Some((participant, installed_revision, binding_revision)) = plan else {
        return Ok(());
    };
    let bindings = OperationStorage {
        repository: format!("trellis_operations_{deployment}"),
        staging: format!("trellis_operation_staging_{deployment}"),
    };
    let jetstream = async_nats::jetstream::new(operational.clone());
    for (bucket, object_store) in [(&bindings.repository, false), (&bindings.staging, true)] {
        let marker = format!(
            "trellis.framework.operation.v1:{deployment}:{}",
            if object_store {
                "staging"
            } else {
                "repository"
            }
        );
        let stream_name = format!("{}_{bucket}", if object_store { "OBJ" } else { "KV" });
        match jetstream.get_stream(&stream_name).await {
            Ok(_) => (),
            Err(error) if matches!(error.kind(),async_nats::jetstream::context::GetStreamErrorKind::JetStream(error) if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND) => {
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
            return Err(AuthError::Conflict);
        }
    }
    store.transaction(|connection| {
        let current: Option<(u64,u64)> = connection.query_row("SELECT b.installed_revision,b.revision FROM auth_deployment_bindings b JOIN auth_deployments d USING(deployment_id) WHERE b.deployment_id=?1 AND d.state='active'",[deployment],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if current != Some((installed_revision,binding_revision)) { return Err(AuthError::Conflict); }
        connection.execute("INSERT INTO auth_resource_binding_evidence(owner_kind,owner_id,participant_id,installed_revision,resource_kind,local_name,binding_id,provider_identity,actual_json,state,materialized_at) VALUES('deployment',?1,?2,?3,'framework_operation','operations',?4,?1,?5,'available',?6) ON CONFLICT(owner_kind,owner_id,participant_id,installed_revision,resource_kind,local_name) DO UPDATE SET actual_json=excluded.actual_json,state='available',materialized_at=excluded.materialized_at",params![deployment,participant,installed_revision,digest(&("framework_operation",deployment))?,json(&bindings)?,now])?;
        Ok(())
    })
}
