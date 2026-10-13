//! Administrative resource discovery and explicit detached-allocation removal.

use super::{
    rpc::{caller, mutation, rpc_error},
    sqlite::{digest, AuthError, Mutation, SqliteAuthorizationStore},
};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Arc;
use trellis_protocol::PlatformPrivilege;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{CursorPage, CursorPageInfo},
    apis::trellis_auth_v1,
    types::*,
};

/// Register resource management on the ordinary platform provider.
pub(crate) fn register_resource_rpc<P: ParticipantDescriptor>(
    provider: &mut trellis_auth_v1::Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
    operational: async_nats::Client,
) {
    let list_store = store.clone();
    provider.register_resources_list(move |context, input| {
        let store = list_store.clone();
        async move {
            let actor =
                caller(&context, Some(PlatformPrivilege::PrincipalsManage)).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_privilege(
                        connection,
                        &actor,
                        "principals.manage",
                    )?;
                    list_resources(connection, &input)
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    let get_store = store.clone();
    provider.register_resources_get(move |context, input| {
        let store = get_store.clone();
        async move {
            let actor =
                caller(&context, Some(PlatformPrivilege::PrincipalsManage)).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_privilege(
                        connection,
                        &actor,
                        "principals.manage",
                    )?;
                    Ok(AuthResourceGetResponse {
                        resource: read_resource(connection, &input.resource_id.to_string())?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    provider.register_resources_remove(move |context, input| {
        let store = store.clone();
        let operational = operational.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(PlatformPrivilege::PrincipalsManage),
                &input,
            )
            .map_err(rpc_error)?;
            remove_resource(
                &store,
                &operational,
                &identity,
                &input,
                time::OffsetDateTime::now_utc().unix_timestamp(),
            )
            .await
            .map_err(rpc_error)
        }
    });
}

pub(super) fn read_resource(
    connection: &Connection,
    id: &str,
) -> Result<AuthResourceBinding, AuthError> {
    let row: Option<(String,String,Option<String>,String,String,u64,String,Option<String>)> = connection.query_row("SELECT owner_kind,owner_id,participant_id,local_name,kind,revision,state,namespace_json FROM auth_resources WHERE resource_id=?1", [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))).optional()?;
    let (owner, owner_id, participant, name, kind, revision, state, namespace) =
        row.ok_or(AuthError::NotFound)?;
    let namespace: serde_json::Value = namespace
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    Ok(AuthResourceBinding {
        resource_id: id.to_string().into(),
        owner_kind: serde_json::from_value(owner.into())?,
        owner_id: owner_id.parse().map_err(|_| AuthError::Unavailable)?,
        participant_id: participant,
        client_id: namespace
            .get("clientId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        browser_origin: namespace
            .get("origin")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        local_name: name,
        kind: if kind == "consumer" {
            AuthResourceKind::EventConsumer
        } else {
            serde_json::from_value(kind.into())?
        },
        revision: AuthRevision(revision.into()),
        state: serde_json::from_value(state.into())?,
        extra: Default::default(),
    })
}

pub(super) fn list_resources(
    connection: &Connection,
    input: &AuthResourcesListRequest,
) -> Result<AuthResourcesListResponse, AuthError> {
    let cursor = input
        .page
        .as_ref()
        .and_then(|page| page.cursor.as_ref())
        .map(|cursor| cursor.as_ref())
        .unwrap_or("");
    let limit = input
        .page
        .as_ref()
        .and_then(|page| page.limit)
        .unwrap_or(50) as usize;
    if limit == 0 || limit > 200 {
        return Err(AuthError::Invalid("invalid page size".into()));
    }
    let mut statement = connection.prepare("SELECT resource_id FROM auth_resources WHERE resource_id>?1 AND (?2 IS NULL OR owner_kind=?2) AND (?3 IS NULL OR owner_id=?3) AND (?4 IS NULL OR state=?4) ORDER BY resource_id LIMIT ?5")?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.owner_kind.as_ref().map(|owner| owner.as_str()),
                input.owner_id.as_ref().map(ToString::to_string),
                input.state.as_ref().map(|state| state.as_str()),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    Ok(CursorPage {
        items: ids
            .iter()
            .map(|id| read_resource(connection, id))
            .collect::<Result<_, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

/// Permanently delete only a detached owned allocation selected by an administrator.
/// Broker failure leaves a durable manual intent; only retrying that same request
/// resumes it. Deployment changes and startup never create or resume removals.
pub(super) async fn remove_resource(
    store: &Arc<SqliteAuthorizationStore>,
    operational: &async_nats::Client,
    identity: &Mutation,
    input: &AuthResourceRemoveRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    let store_copy = store.clone();
    let identity_copy = identity.clone();
    let input_copy = input.clone();
    let execution_owner = super::sqlite::id();
    let claim_owner = execution_owner.clone();
    let (result, command) = tokio::task::spawn_blocking(move || store_copy.transaction(|connection| {
        SqliteAuthorizationStore::require_privilege(connection, &identity_copy.actor, "principals.manage")?;
        let result = store_copy.mutate_in_transaction(connection,&identity_copy,"rpc.resources.remove",&input_copy,now,|connection| {
            let id = input_copy.resource_id.to_string();
            let resource = read_resource(connection,&id)?;
            if resource.state != AuthResourceState::Detached || resource.revision != input_copy.expected_revision { return Err(AuthError::Conflict); }
            let kind = if resource.kind == AuthResourceKind::EventConsumer { "consumer" } else { resource.kind.as_str() };
            let expected_id = if resource.owner_kind.as_str() == "deployment" {
                digest(&(resource.owner_kind.as_str(),resource.owner_id.to_string(),&resource.participant_id,kind,&resource.local_name))?
            } else if resource.owner_kind.as_str() == "user" && resource.kind == AuthResourceKind::State {
                let namespace:String=connection.query_row("SELECT namespace_json FROM auth_resources WHERE resource_id=?1",[&id],|row|row.get(0))?;
                digest(&("trellis.state",resource.owner_id.to_string(),serde_json::from_str::<serde_json::Value>(&namespace)?,&resource.local_name))?
            } else { return Err(AuthError::Conflict); };
            if expected_id != id { return Err(AuthError::Conflict); }
            let (physical, allocation): (String,String) = connection.query_row("SELECT physical_id,allocation_id FROM auth_resources WHERE resource_id=?1",[&id],|row|Ok((row.get(0)?,row.get(1)?)))?;
            let owned_name = format!("TRELLIS_RESOURCE_{id}_{allocation}");
            let physical_matches = match resource.kind {
                AuthResourceKind::Kv | AuthResourceKind::Store => physical == owned_name,
                AuthResourceKind::State => physical == format!("{id}.{allocation}"),
                AuthResourceKind::EventConsumer => serde_json::from_str::<serde_json::Value>(&physical).is_ok_and(|value| value["consumer"] == owned_name && value["replay_consumer"] == format!("{owned_name}_replay") && value["stream"] == crate::resources::EVENT_STREAM),
                AuthResourceKind::Job => serde_json::from_str::<serde_json::Value>(&physical).is_ok_and(|value|value["consumer"] == owned_name && value["work_stream"] == crate::resources::JOBS_WORK_STREAM && value["namespace"].as_str() == digest(&("jobs",resource.owner_id.to_string())).ok().as_deref()),
                AuthResourceKind::Unknown(_) => false,
            };
            if !physical_matches {return Err(AuthError::Conflict);}
            let revision = resource.revision.0.0 + 1;
            if connection.execute("UPDATE auth_resources SET state='destroying',revision=?2,updated_at=?3 WHERE resource_id=?1 AND state='detached' AND revision=?4",params![id,revision,now,input_copy.expected_revision.0.0])? != 1 {return Err(AuthError::Conflict);}
            connection.execute("INSERT INTO auth_resource_history SELECT resource_id,revision,commitment_json,actual_json,state,?2 FROM auth_resources WHERE resource_id=?1",params![id,now])?;
            connection.execute("INSERT INTO auth_resource_removals(actor_id,request_id,resource_id,resource_revision,physical_id,allocation_id,state) VALUES(?1,?2,?3,?4,?5,?6,'accepted')",params![identity_copy.actor,identity_copy.request_id,id,revision,physical,allocation])?;
            Ok(AuthMutationResult { revision: AuthRevision(revision.into()), enforcement: None, extra: Default::default() })
        })?;
        let command: (String,u64,String,String,String) = connection.query_row("SELECT resource_id,resource_revision,physical_id,state,allocation_id FROM auth_resource_removals WHERE actor_id=?1 AND request_id=?2",params![identity_copy.actor,identity_copy.request_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        if command.3 != "completed" {
            if !connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_resources WHERE resource_id=?1 AND revision=?2 AND allocation_id=?3 AND state='destroying')",params![command.0,command.1,command.4],|row|row.get::<_,bool>(0))? {return Err(AuthError::Conflict);}
            // Only an explicit retry of this command can recover an interrupted
            // execution. A new request cannot adopt its durable removal intent.
            if connection.execute("UPDATE auth_resource_removals SET state='executing',execution_owner=?3,lease_until=?4 WHERE actor_id=?1 AND request_id=?2 AND (state='accepted' OR (state='executing' AND lease_until<=?5))",params![identity_copy.actor,identity_copy.request_id,claim_owner,now+30,now])? != 1 {return Err(AuthError::Conflict);}
        }
        Ok((result,command))
    })).await.map_err(|_|AuthError::Unavailable)??;
    let (id, revision, physical, state, allocation) = command;
    if state == "completed" {
        return Ok(result);
    }
    let jetstream = async_nats::jetstream::new(operational.clone());
    let deletion: Result<(),AuthError> = async {
    let (kind, local_name, owner_id, owner_kind): (String,String,String,String) = store.transaction(|connection| {
        connection.query_row("SELECT kind,local_name,owner_id,owner_kind FROM auth_resources WHERE resource_id=?1 AND allocation_id=?2 AND revision=?3 AND state='destroying'",params![id,allocation,revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).map_err(AuthError::from)
    })?;
    let marker = format!("trellis.resource.v1:{id}:{allocation}");
    if kind == "state" {
        // This key includes the allocation incarnation. A delayed old command
        // cannot purge a replacement allocation's value.
        let state = jetstream.get_key_value("trellis_state").await.map_err(|error|AuthError::Broker(error.to_string()))?;
        if state.status().await.map_err(|error|AuthError::Broker(error.to_string()))?.info.config.description.as_deref() != Some(crate::platform::state::OWNERSHIP_MARKER) { return Err(AuthError::Conflict); }
        if owner_kind == "deployment" {
            jetstream.get_stream("KV_trellis_state").await.map_err(|error|AuthError::Broker(error.to_string()))?.purge().filter(format!("$KV.trellis_state.{physical}.>")).await.map_err(|error|AuthError::Broker(error.to_string()))?;
        } else {
            state.purge(&physical).await.map_err(|error|AuthError::Broker(error.to_string()))?;
        }
    } else if kind == "job" || kind == "consumer" {
        let mut value: serde_json::Value = serde_json::from_str(&physical)?;
        let owned = value.get("owned").unwrap_or(&value).clone();
        let consumer = owned["consumer"].as_str().ok_or(AuthError::Conflict)?;
        let mut targets = vec![(if kind == "job" {crate::resources::JOBS_WORK_STREAM} else {crate::resources::EVENT_STREAM}, consumer.to_owned())];
        if kind == "consumer" {
            targets.push((trellis_events_runtime::REPLAY_STREAM,owned["replay_consumer"].as_str().ok_or(AuthError::Conflict)?.to_owned()));
        }
        // Check every retained durable before deleting either member of a pair.
        for (stream_name,name) in &targets {
            let stream = jetstream.get_stream(*stream_name).await.map_err(|error|AuthError::Broker(error.to_string()))?;
            match stream.consumer_info(name).await {
                Ok(info) if info.config.description.as_deref() == Some(&marker) => (),
                Ok(_) => return Err(AuthError::Conflict),
                Err(error) if error.kind() == async_nats::jetstream::context::ConsumerInfoErrorKind::NotFound => (),
                Err(error) => return Err(AuthError::Broker(error.to_string())),
            }
        }
        if kind == "job" && value.get("purges").is_none() {
            let namespace = owned["namespace"].as_str().ok_or(AuthError::Conflict)?;
            let mut purges = Vec::new();
            for (stream_name,filter) in [
                (crate::resources::JOBS_STREAM.to_owned(),format!("{}.>",owned["publish_prefix"].as_str().ok_or(AuthError::Conflict)?)),
                (crate::resources::JOBS_WORK_STREAM.to_owned(),owned["work_subject"].as_str().ok_or(AuthError::Conflict)?.to_owned()),
                (format!("KV_JOBS_KEYS_{namespace}"),format!("$KV.JOBS_KEYS_{namespace}.{namespace}.{local_name}.>")),
            ] {
                match jetstream.get_stream(&stream_name).await {
                    Ok(stream) => {
                        let info = stream.cached_info();
                        if stream_name.starts_with("KV_") && info.config.description.as_deref() != Some(format!("trellis.jobs.keys.v1:{owner_id}").as_str()) { return Err(AuthError::Conflict); }
                        purges.push(serde_json::json!({"stream":stream_name,"filter":filter,"before":info.state.last_sequence.checked_add(1).ok_or(AuthError::Unavailable)?}));
                    }
                    Err(error) if matches!(error.kind(),async_nats::jetstream::context::GetStreamErrorKind::JetStream(error) if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND) => (),
                    Err(error) => return Err(AuthError::Broker(error.to_string())),
                }
            }
            value = serde_json::json!({"owned":owned,"purges":purges});
            // Capture the cutoff once under the existing lease before destruction.
            // Every retry uses this same cutoff, including a delayed execution.
            store.transaction(|connection| {
                if connection.execute("UPDATE auth_resource_removals SET physical_id=?4 WHERE actor_id=?1 AND request_id=?2 AND execution_owner=?3 AND state='executing' AND physical_id=?5",params![identity.actor,identity.request_id,execution_owner,serde_json::to_string(&value)?,physical])? != 1 { return Err(AuthError::Conflict); }
                Ok(())
            })?;
        }
        for (stream_name,name) in targets {
            let stream = jetstream.get_stream(stream_name).await.map_err(|error|AuthError::Broker(error.to_string()))?;
            match stream.delete_consumer(&name).await {
                Ok(_) => (),
                Err(error) if matches!(error.kind(),async_nats::jetstream::stream::ConsumerErrorKind::JetStream(error) if error.error_code()==async_nats::jetstream::ErrorCode::CONSUMER_NOT_FOUND) => (),
                Err(error) => return Err(AuthError::Broker(error.to_string())),
            }
        }
        if let Some(purges) = value.get("purges").and_then(serde_json::Value::as_array) {
            for purge in purges {
                let stream = jetstream.get_stream(purge["stream"].as_str().ok_or(AuthError::Conflict)?).await.map_err(|error|AuthError::Broker(error.to_string()))?;
                stream.purge().filter(purge["filter"].as_str().ok_or(AuthError::Conflict)?).sequence(purge["before"].as_u64().ok_or(AuthError::Conflict)?).await.map_err(|error|AuthError::Broker(error.to_string()))?;
            }
        }
    } else { match jetstream.get_stream(format!("{}_{physical}",if kind == "store" {"OBJ"} else {"KV"})).await {
        Ok(mut stream) => {
            if stream
                .info()
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?
                .config
                .description
                .as_deref()
                != Some(format!("trellis.resource.v1:{id}:{allocation}").as_str())
            {
                return Err(AuthError::Conflict);
            }
            if kind == "store" {
                jetstream.delete_object_store(&physical).await.map_err(|error|AuthError::Broker(error.to_string()))?;
            } else { jetstream
                .delete_key_value(&physical)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?; }
        }
        Err(error) if matches!(error.kind(),async_nats::jetstream::context::GetStreamErrorKind::JetStream(error) if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND) =>
            {}
        Err(error) => return Err(AuthError::Broker(error.to_string())),
    } }
    Ok(()) }.await;
    if let Err(error) = deletion {
        // Release only this execution's claim; cancellation instead leaves its
        // bounded lease for the same manual request to recover.
        let store = store.clone();
        let identity = identity.clone();
        tokio::task::spawn_blocking(move || store.transaction(|connection| {
            connection.execute("UPDATE auth_resource_removals SET state='accepted',execution_owner=NULL,lease_until=NULL WHERE actor_id=?1 AND request_id=?2 AND state='executing' AND execution_owner=?3",params![identity.actor,identity.request_id,execution_owner])?;
            Ok(())
        })).await.map_err(|_|AuthError::Unavailable)??;
        return Err(error);
    }
    let store = store.clone();
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || store.transaction(|connection| {
        if connection.execute("UPDATE auth_resource_removals SET state='completed',execution_owner=NULL,lease_until=NULL WHERE actor_id=?1 AND request_id=?2 AND allocation_id=?3 AND resource_revision=?4 AND state='executing' AND execution_owner=?5",params![identity.actor,identity.request_id,allocation,revision,execution_owner])? != 1 {return Err(AuthError::Conflict);}
        if !connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_resources WHERE resource_id=?1 AND allocation_id=?2 AND revision=?3 AND state='destroying')",params![id,allocation,revision],|row|row.get::<_,bool>(0))? {return Err(AuthError::Conflict);}
        connection.execute("DELETE FROM auth_resource_binding_evidence WHERE binding_id=?1",[&id])?;
        connection.execute("DELETE FROM auth_resources WHERE resource_id=?1 AND revision=?2 AND allocation_id=?3 AND state='destroying'",params![id,revision,allocation])?;
        Ok(())
    })).await.map_err(|_|AuthError::Unavailable)??;
    Ok(result)
}

#[cfg(test)]
#[path = "rpc_resources_tests.rs"]
mod tests;
