//! Capability-scoped resource evidence for independently deployed runtime roles.

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::params;
use sha2::{Digest, Sha256};
use trellis_runtime_apis::{
    __types::{CursorPage, CursorPageInfo},
    apis::trellis_auth_v1,
    types::*,
};

use super::{
    rpc::{caller, rpc_error},
    sqlite::decode,
    AuthError, SqliteAuthorizationStore,
};

pub(super) fn register<P: trellis_rs::generated::ParticipantDescriptor>(
    provider: &mut trellis_auth_v1::Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    provider.register_resource_bindings_query(move |context, input| {
        let store = store.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            let capability = match &input.kind {
                AuthResourceEvidenceKind::JobQueue => "trellis.auth@v1::jobResourceBindingsRead",
                AuthResourceEvidenceKind::EventConsumer => "trellis.auth@v1::eventResourceBindingsRead",
                _ => return Err(rpc_error(AuthError::Denied)),
            };
            // Both capabilities authorize the same route, not each other's data.
            let authorized = context.request().verified_request().is_some_and(|request| {
                request.authority().authority().capabilities.iter()
                    .any(|granted| granted.capability_id == capability)
            });
            if !authorized { return Err(rpc_error(AuthError::Denied)); }
             tokio::task::spawn_blocking(move || store.transaction(|connection| {
                let projection = input.instance_id.as_ref().map(|instance|super::native_projection::project_instance(connection,&instance.to_string(),time::OffsetDateTime::now_utc().unix_timestamp())).transpose()?;
                if let Some(projection)=&projection {
                    if input.owner_id.as_ref().is_some_and(|owner|owner.to_string()!=projection.deployment_id)
                      || input.participant_id.as_ref().is_some_and(|participant|*participant!=projection.participant_id) {return Err(AuthError::Denied);}
                }
                let owner_id = input.owner_id.as_ref().map(ToString::to_string).or_else(||projection.as_ref().map(|projection|projection.deployment_id.clone()));
                let cursor = input.page.as_ref().and_then(|page| page.cursor.as_deref())
                    .map(|cursor| cursor.parse::<i64>())
                    .transpose().map_err(|_| AuthError::Invalid("invalid cursor".into()))?
                    .unwrap_or(0);
                let limit = input.page.as_ref().and_then(|page| page.limit).unwrap_or(50) as usize;
                if cursor < 0 || !(1..=200).contains(&limit) {
                    return Err(AuthError::Invalid("invalid page".into()));
                }
                let mut statement = connection.prepare(
                    "SELECT e.rowid,e.binding_id,e.owner_id,e.participant_id,e.installed_revision,e.local_name,e.state,e.provider_identity,e.actual_json,
                     CASE WHEN e.state='available' AND r.state='ready' AND r.binding_revision=b.revision AND b.installed_revision=e.installed_revision AND d.state='active' THEN b.resource_commitments_json ELSE NULL END
                     FROM auth_resource_binding_evidence e
                     LEFT JOIN auth_deployment_bindings b ON b.deployment_id=e.owner_id
                     LEFT JOIN auth_deployments d ON d.deployment_id=e.owner_id
                     LEFT JOIN auth_resources r ON r.resource_id=e.binding_id
                     WHERE e.owner_kind='deployment' AND e.resource_kind=?1 AND e.rowid>?2 AND e.actual_json IS NOT NULL
                       AND (?3 IS NULL OR e.owner_id=?3) AND (?4 IS NULL OR e.participant_id=?4)
                       AND (?5 IS NULL OR e.local_name=?5) AND (?6 IS NULL OR e.binding_id=?6)
                     ORDER BY e.rowid LIMIT ?7"
                )?;
                let mut rows = statement.query_map(params![
                    input.kind.as_str(), cursor, owner_id,
                    input.participant_id, input.local_name, input.resource_id.as_ref().map(|id| id.as_ref()), limit + 1,
                ], |row| Ok((
                    row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,
                    row.get::<_,u64>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,String>(7)?,
                    row.get::<_,Option<String>>(8)?,row.get::<_,Option<String>>(9)?,
                )))?.collect::<rusqlite::Result<Vec<_>>>()?;
                let next_cursor = (rows.len()>limit).then(|| rows[limit-1].0.to_string());
                rows.truncate(limit);
                let mut items = Vec::with_capacity(rows.len());
                 for (_, resource_id, owner, participant_id, revision, local_name, state, physical, actual, commitments) in rows {
                    if let Some(projection)=&projection {
                        let entries = match &input.kind {
                            AuthResourceEvidenceKind::JobQueue => projection.resources.get("jobs").and_then(|jobs|jobs.get("queues")),
                            AuthResourceEvidenceKind::EventConsumer => projection.resources.get("eventConsumers"),
                            _ => None,
                        };
                        if !entries.is_some_and(|entries|entries.get(&local_name).is_some()) {continue;}
                    }
                    let actual = actual.ok_or(AuthError::Unavailable)?;
                    let resource_bindings_digest = commitments.map(|json| -> Result<AuthDigest, AuthError> {
                        let value: serde_json::Value = if let Some(projection)=&projection {projection.resources.clone()} else {decode(&json)?};
                        let canonical = trellis_protocol::canonicalize_json(&value)?;
                        Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())).into())
                    }).transpose()?;
                    let (job, event_consumer) = match &input.kind {
                        AuthResourceEvidenceKind::JobQueue => {
                            let binding: trellis_rs::service::JobsQueueResourceBinding = decode(&actual)?;
                            let physical: serde_json::Value = decode(&physical)?;
                            let namespace = physical.get("namespace").and_then(serde_json::Value::as_str)
                                .ok_or(AuthError::Unavailable)?.to_owned();
                            let work_stream = physical.get("work_stream").and_then(serde_json::Value::as_str)
                                .ok_or(AuthError::Unavailable)?.to_owned();
                            (Some(AuthJobResourceEvidence {namespace,work_stream,consumer:binding.consumer_name,
                                publish_prefix:binding.publish_prefix,work_subject:binding.work_subject,extra:Default::default()}),None)
                        }
                        AuthResourceEvidenceKind::EventConsumer => {
                            let binding: trellis_rs::service::EventConsumerResourceBinding = decode(&actual)?;
                            (None,Some(AuthEventConsumerEvidence {stream:binding.stream,consumer:binding.consumer_name,
                                replay_consumer:binding.replay_binding.consumer_name,filter_subjects:binding.filter_subjects,extra:Default::default()}))
                        }
                        _ => return Err(AuthError::Denied),
                    };
                    items.push(AuthResourceEvidence {resource_id:resource_id.into(),owner_id:owner.parse().map_err(|_|AuthError::Unavailable)?,
                        participant_id,installed_revision:AuthRevision(revision.into()),local_name,kind:input.kind.clone(),
                        state:serde_json::from_value(state.into())?,resource_bindings_digest,job,event_consumer,extra:Default::default()});
                }
                Ok(CursorPage {items,page:CursorPageInfo {next_cursor}})
            })).await.map_err(|_|rpc_error(AuthError::Unavailable))?.map_err(rpc_error)
        }
    });
}
