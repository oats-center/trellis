//! Startup materialization of installed native KV declarations.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_idl::{compile_evidence, PackageEvidence, ResourceDefinition};
use trellis_rs::service::{KvResourceBinding, ServiceResourceBindings};

use super::sqlite::{decode, digest, json, AuthError, SqliteAuthorizationStore};

/// Detach omitted allocations without deleting any provider data.
pub(super) fn detach_deployment_resources(
    connection: &rusqlite::Connection,
    deployment: &str,
    keep: &BTreeSet<String>,
    now: i64,
) -> Result<(), AuthError> {
    let mut statement = connection.prepare("SELECT resource_id,local_name,state FROM auth_resources WHERE owner_kind='deployment' AND owner_id=?1")?;
    let resources = statement
        .query_map([deployment], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (resource, name, state) in resources {
        if keep.contains(&name) || state == "detached" || state == "destroying" {
            continue;
        }
        // A provider allocation in flight must finish before deployment changes.
        if state == "pending" {
            return Err(AuthError::Conflict);
        }
        connection.execute("UPDATE auth_resources SET state='detached',revision=revision+1,updated_at=?2 WHERE resource_id=?1", params![resource,now])?;
        connection.execute("UPDATE auth_resource_binding_evidence SET state='unavailable',materialized_at=?2 WHERE binding_id=?1", params![resource,now])?;
        connection.execute("INSERT INTO auth_resource_history SELECT resource_id,revision,commitment_json,actual_json,state,?2 FROM auth_resources WHERE resource_id=?1", params![resource,now])?;
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct KvAllocation {
    resource_id: String,
    history: i64,
    ttl_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct ResourcePlan {
    principal: String,
    deployment: String,
    participant: String,
    installed_revision: u64,
    binding_revision: u64,
    kv: BTreeMap<String, KvAllocation>,
}

/// Materialize installed declarations using the operational connection, then
/// commit actual bindings before any native provider is admitted. This is a
/// startup-owner operation, not a caller authorization or allocation API.
pub(crate) async fn materialize_native_kv(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    deployment: &str,
    now: i64,
) -> Result<ServiceResourceBindings, AuthError> {
    let plan = store.transaction(|connection| {
        let (principal,participant,installed_revision,binding_revision,evidence):(String,String,u64,u64,String)=connection.query_row(
            "SELECT i.principal_id,d.participant_id,b.installed_revision,b.revision,p.projection_json FROM auth_deployments d JOIN auth_deployment_bindings b USING(deployment_id) JOIN auth_installed_participants p ON p.participant_id=d.participant_id AND p.revision=b.installed_revision JOIN auth_instances i ON i.deployment_id=d.deployment_id AND i.installed_revision=b.installed_revision JOIN auth_principals a ON a.principal_id=i.principal_id WHERE d.deployment_id=?1 AND d.state='active' AND i.state='active' AND a.state='active'",
            [deployment],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        let evidence:PackageEvidence=decode(&evidence)?;
        let graph=compile_evidence(evidence).map_err(|error|AuthError::Invalid(error.to_string()))?;
        let definition=graph.root_package().participants().values().find(|definition|definition.identity().as_str()==participant).ok_or(AuthError::NotFound)?;
        let mut kv=BTreeMap::new();
        for (name, declaration) in definition.resources() {
            let ResourceDefinition::Kv {history,ttl_ms,..}=declaration else {
                return Err(AuthError::Invalid("startup KV materialization cannot allocate this resource kind".into()));
            };
            let name=name.to_string();
            kv.insert(name.clone(),KvAllocation {
                resource_id:digest(&("deployment",deployment,&participant,"kv",&name))?,
                history:i64::try_from(*history).map_err(|_|AuthError::Denied)?,
                ttl_ms:i64::try_from(*ttl_ms).map_err(|_|AuthError::Denied)?,
            });
        }
        for (name, allocation) in &kv {
            let current: Option<String> = connection.query_row("SELECT state FROM auth_resources WHERE resource_id=?1", [&allocation.resource_id], |row| row.get(0)).optional()?;
            if current.as_deref() == Some("destroying") { return Err(AuthError::Conflict); }
            connection.execute("INSERT INTO auth_resources(resource_id,owner_kind,owner_id,participant_id,kind,local_name,commitment_json,state,binding_revision,revision,created_at,updated_at,physical_id) VALUES(?1,'deployment',?2,?3,'kv',?4,?5,'pending',?6,1,?7,?7,?8) ON CONFLICT(resource_id) DO UPDATE SET state='pending',updated_at=excluded.updated_at",params![allocation.resource_id,deployment,participant,name,json(allocation)?,binding_revision,now,format!("TRELLIS_RESOURCE_{}",allocation.resource_id)])?;
        }
        Ok(ResourcePlan {principal,deployment:deployment.into(),participant,installed_revision,binding_revision,kv})
    })?;
    let jetstream = async_nats::jetstream::new(operational.clone());
    // Empty projections are valid only after inspecting the installed declaration set.
    let mut bindings = ServiceResourceBindings::default();
    let allocation_result: Result<(),AuthError> = async {
    for (name, allocation) in &plan.kv {
        let bucket = format!("TRELLIS_RESOURCE_{}", allocation.resource_id);
        let marker = format!("trellis.resource.v1:{}", allocation.resource_id);
        let resource = match jetstream.get_stream(format!("KV_{bucket}")).await {
            Ok(_) => jetstream
                .get_key_value(&bucket)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?,
            Err(error) if matches!(error.kind(),async_nats::jetstream::context::GetStreamErrorKind::JetStream(error) if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND) => {
                jetstream
                    .create_key_value(async_nats::jetstream::kv::Config {
                        bucket: bucket.clone(),
                        description: marker.clone(),
                        history: allocation.history,
                        max_age: Duration::from_millis(allocation.ttl_ms as u64),
                        storage: async_nats::jetstream::stream::StorageType::File,
                        ..Default::default()
                    })
                    .await
                    .map_err(|error| AuthError::Broker(error.to_string()))?
            }
            Err(error) => return Err(AuthError::Broker(error.to_string())),
        };
        let mut status = resource
            .status()
            .await
            .map_err(|error| AuthError::Broker(error.to_string()))?;
        if status.info.config.description.as_deref() != Some(marker.as_str())
            || status.info.config.storage != async_nats::jetstream::stream::StorageType::File
            || status.info.config.subjects != vec![format!("$KV.{bucket}.>")]
        {
            return Err(AuthError::Conflict);
        }
        let desired_ttl = Duration::from_millis(allocation.ttl_ms as u64);
        if allocation.history < status.history() || (desired_ttl != Duration::ZERO && (status.max_age() == Duration::ZERO || desired_ttl < status.max_age())) {
            return Err(AuthError::Invalid("potential_data_loss".into()));
        }
        if allocation.history != status.history() || desired_ttl != status.max_age() {
            let mut config = status.info.config.clone();
            config.max_messages_per_subject = allocation.history;
            config.max_age = desired_ttl;
            jetstream.update_stream(config).await.map_err(|error| AuthError::Broker(error.to_string()))?;
            status = resource.status().await.map_err(|error| AuthError::Broker(error.to_string()))?;
        }
        bindings.kv.insert(
            name.clone(),
            KvResourceBinding {
                bucket,
                history: status.history(),
                ttl_ms: allocation.ttl_ms,
                // Capacity desires are not hard limits. Report the broker's actual limit.
                max_value_bytes: (status.info.config.max_message_size > 0)
                    .then_some(status.info.config.max_message_size as i64),
            },
        );
    }
    Ok(())
    }.await;
    if let Err(error) = allocation_result {
        store.transaction(|connection| {
            connection.execute("UPDATE auth_resources SET state='failed',revision=revision+1,updated_at=?2 WHERE owner_kind='deployment' AND owner_id=?1 AND state='pending'",params![plan.deployment,now])?;
            connection.execute("UPDATE auth_resource_binding_evidence SET state='unavailable' WHERE owner_kind='deployment' AND owner_id=?1",[&plan.deployment])?;
            SqliteAuthorizationStore::policy_changed(connection,&super::EnforcementScope::Deployment(plan.deployment.clone()),now)?;
            Ok(())
        })?;
        return Err(error);
    }
    store.transaction(|connection| {
        let current:Option<(u64,u64)>=connection.query_row("SELECT b.installed_revision,b.revision FROM auth_deployment_bindings b JOIN auth_deployments d USING(deployment_id) JOIN auth_principals p ON p.principal_id=?2 WHERE b.deployment_id=?1 AND d.state='active' AND p.state='active'",params![plan.deployment,plan.principal],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if current!=Some((plan.installed_revision,plan.binding_revision)) {return Err(AuthError::Conflict);}
        for (name,allocation) in &plan.kv {
            let binding=&bindings.kv[name];
            let commitment=json(allocation)?;
            let actual=json(binding)?;
            if connection.execute("UPDATE auth_resources SET commitment_json=?2,physical_id=?3,actual_json=?4,state='ready',binding_revision=?5,revision=revision+1,updated_at=?6 WHERE resource_id=?1 AND state='pending'",params![allocation.resource_id,commitment,binding.bucket,actual,plan.binding_revision,now])? != 1 {return Err(AuthError::Conflict);}
            connection.execute("INSERT INTO auth_resource_history SELECT resource_id,revision,commitment_json,actual_json,state,?2 FROM auth_resources WHERE resource_id=?1",params![allocation.resource_id,now])?;
            connection.execute("UPDATE auth_resource_binding_evidence SET state='unavailable' WHERE binding_id=?1",[&allocation.resource_id])?;
            connection.execute("INSERT INTO auth_resource_binding_evidence(owner_kind,owner_id,participant_id,installed_revision,resource_kind,local_name,binding_id,provider_identity,actual_json,state,materialized_at) VALUES('deployment',?1,?2,?3,'kv',?4,?5,?6,?7,'available',?8) ON CONFLICT(owner_kind,owner_id,participant_id,installed_revision,resource_kind,local_name) DO UPDATE SET provider_identity=excluded.provider_identity,actual_json=excluded.actual_json,state='available',materialized_at=excluded.materialized_at",params![plan.deployment,plan.participant,plan.installed_revision,name,allocation.resource_id,binding.bucket,actual,now])?;
        }
        connection.execute("UPDATE auth_deployment_bindings SET resource_commitments_json=?1 WHERE deployment_id=?2",params![json(&bindings)?,plan.deployment])?;
        Ok(bindings.clone())
    })
}
