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
    let row: Option<(String,String,String,String,String,u64,String)> = connection.query_row("SELECT owner_kind,owner_id,participant_id,local_name,kind,revision,state FROM auth_resources WHERE resource_id=?1", [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).optional()?;
    let (owner, owner_id, participant, name, kind, revision, state) =
        row.ok_or(AuthError::NotFound)?;
    Ok(AuthResourceBinding {
        resource_id: id.to_string().into(),
        owner_kind: serde_json::from_value(owner.into())?,
        owner_id: owner_id.parse().map_err(|_| AuthError::Unavailable)?,
        participant_id: participant,
        local_name: name,
        kind: serde_json::from_value(kind.into())?,
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

/// Permanently delete only a detached owned KV selected by an administrator.
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
    let (result, command) = tokio::task::spawn_blocking(move || store_copy.transaction(|connection| {
        SqliteAuthorizationStore::require_privilege(connection, &identity_copy.actor, "principals.manage")?;
        let result = store_copy.mutate_in_transaction(connection,&identity_copy,"rpc.resources.remove",&input_copy,now,|connection| {
            let id = input_copy.resource_id.to_string();
            let resource = read_resource(connection,&id)?;
            if resource.state != AuthResourceState::Detached || resource.revision != input_copy.expected_revision { return Err(AuthError::Conflict); }
            if resource.kind != AuthResourceKind::Kv { return Err(AuthError::Invalid("unsupported resource provider".into())); }
            if digest(&(resource.owner_kind.as_str(),resource.owner_id.to_string(),&resource.participant_id,"kv",&resource.local_name))? != id { return Err(AuthError::Conflict); }
            let physical: String = connection.query_row("SELECT physical_id FROM auth_resources WHERE resource_id=?1",[&id],|row|row.get(0))?;
            if physical != format!("TRELLIS_RESOURCE_{id}") {return Err(AuthError::Conflict);}
            let revision = resource.revision.0.0 + 1;
            if connection.execute("UPDATE auth_resources SET state='destroying',revision=?2,updated_at=?3 WHERE resource_id=?1 AND state='detached' AND revision=?4",params![id,revision,now,input_copy.expected_revision.0.0])? != 1 {return Err(AuthError::Conflict);}
            connection.execute("INSERT INTO auth_resource_history SELECT resource_id,revision,commitment_json,actual_json,state,?2 FROM auth_resources WHERE resource_id=?1",params![id,now])?;
            connection.execute("INSERT INTO auth_resource_removals VALUES(?1,?2,?3,?4,?5,'accepted')",params![identity_copy.actor,identity_copy.request_id,id,revision,physical])?;
            Ok(AuthMutationResult { revision: AuthRevision(revision.into()), enforcement: None, extra: Default::default() })
        })?;
        let command: (String,u64,String,String) = connection.query_row("SELECT resource_id,resource_revision,physical_id,state FROM auth_resource_removals WHERE actor_id=?1 AND request_id=?2",params![identity_copy.actor,identity_copy.request_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        if command.3 != "completed" && !connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_resources WHERE resource_id=?1 AND revision=?2 AND state='destroying')",params![command.0,command.1],|row|row.get::<_,bool>(0))? {return Err(AuthError::Conflict);}
        Ok((result,command))
    })).await.map_err(|_|AuthError::Unavailable)??;
    let (id, revision, physical, state) = command;
    if state == "completed" {
        return Ok(result);
    }
    let jetstream = async_nats::jetstream::new(operational.clone());
    match jetstream.get_stream(format!("KV_{physical}")).await {
        Ok(mut stream) => {
            if stream
                .info()
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?
                .config
                .description
                .as_deref()
                != Some(format!("trellis.resource.v1:{id}").as_str())
            {
                return Err(AuthError::Conflict);
            }
            jetstream
                .delete_key_value(&physical)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?;
        }
        Err(error) if matches!(error.kind(),async_nats::jetstream::context::GetStreamErrorKind::JetStream(error) if error.error_code()==async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND) =>
            {}
        Err(error) => return Err(AuthError::Broker(error.to_string())),
    }
    let store = store.clone();
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || store.transaction(|connection| {
        connection.execute("DELETE FROM auth_resource_binding_evidence WHERE binding_id=?1",[&id])?;
        connection.execute("DELETE FROM auth_resources WHERE resource_id=?1 AND revision=?2 AND state='destroying'",params![id,revision])?;
        connection.execute("UPDATE auth_resource_removals SET state='completed' WHERE actor_id=?1 AND request_id=?2",params![identity.actor,identity.request_id])?;
        Ok(())
    })).await.map_err(|_|AuthError::Unavailable)??;
    Ok(result)
}

#[cfg(test)]
#[path = "rpc_resources_tests.rs"]
mod tests;
