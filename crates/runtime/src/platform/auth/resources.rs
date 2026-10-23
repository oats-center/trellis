//! Startup materialization of installed native resource declarations.

use async_nats::jetstream::{context::GetStreamErrorKind, ErrorCode};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_idl::{compile_evidence, PackageEvidence, ResourceDefinition};
use trellis_rs::service::{
    EventConsumerResourceBinding, KvResourceBinding, ServiceResourceBindings, StoreResourceBinding,
};

use super::sqlite::{decode, digest, json, AuthError, SqliteAuthorizationStore};

/// Detach omitted allocations without deleting any provider data.
pub(super) fn detach_deployment_resources(
    connection: &crate::platform::auth::sqlite::Connection,
    deployment: &str,
    keep: &BTreeSet<(String, String)>,
    now: i64,
) -> Result<(), AuthError> {
    let mut statement = connection.prepare(
        "SELECT resource_id,kind,local_name,state \
        FROM auth_resources \
        WHERE owner_kind='deployment' \
        AND owner_id=?1",
    )?;
    let resources = statement
        .query_map([deployment], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (resource, kind, name, state) in resources {
        if keep.contains(&(kind, name)) || state == "detached" || state == "destroying" {
            continue;
        }
        // A provider allocation in flight must finish before deployment changes.
        if state == "pending" {
            return Err(AuthError::Conflict);
        }
        connection.execute(
            "UPDATE auth_resources \
            SET state='detached',revision=revision+1,updated_at=?2 \
            WHERE resource_id=?1",
            params![resource, now],
        )?;
        connection.execute(
            "UPDATE auth_resource_binding_evidence \
            SET state='unavailable',materialized_at=?2 \
            WHERE binding_id=?1",
            params![resource, now],
        )?;
        connection.execute(
            "INSERT INTO auth_resource_history SELECT resource_id,revision,\
            commitment_json,actual_json,state,?2 \
            FROM auth_resources \
            WHERE resource_id=?1",
            params![resource, now],
        )?;
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct NativeAllocation {
    kind: String,
    resource_id: String,
    allocation_id: String,
    history: i64,
    ttl_ms: i64,
    consumer: Option<EventConsumerResourceBinding>,
    job: Option<trellis_rs::service::JobsQueueResourceBinding>,
    state: Option<super::state::StateDeclaration>,
}

#[derive(Serialize, Deserialize)]
struct ResourcePlan {
    principal: String,
    deployment: String,
    participant: String,
    installed_revision: u64,
    binding_revision: u64,
    kv: BTreeMap<String, NativeAllocation>,
    materialized: BTreeSet<String>,
    key_bucket_materialized: bool,
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
        let (principal, participant, installed_revision, binding_revision, evidence): (
            String,
            String,
            u64,
            u64,
            String,
        ) = connection.query_row(
            "SELECT i.principal_id,d.participant_id,b.installed_revision,b.revision,\
            p.projection_json \
            FROM auth_deployments d \
            JOIN auth_deployment_bindings b USING(deployment_id) \
            JOIN auth_installed_participants p ON \
            p.participant_id=d.participant_id \
            AND p.revision=b.installed_revision \
            JOIN auth_instances i ON i.deployment_id=d.deployment_id \
            JOIN auth_principals a ON a.principal_id=i.principal_id \
            WHERE d.deployment_id=?1 \
            AND d.state='active' \
            AND i.state='active' \
            AND a.state='active'",
            [deployment],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let evidence: PackageEvidence = decode(&evidence)?;
        let graph =
            compile_evidence(evidence).map_err(|error| AuthError::Invalid(error.to_string()))?;
        let definition = graph
            .root_package()
            .participants()
            .values()
            .find(|definition| definition.identity().as_str() == participant)
            .ok_or(AuthError::NotFound)?;
        let mut kv = BTreeMap::new();
        for (name, declaration) in definition.resources() {
            let (kind, history, ttl_ms) = match declaration {
                ResourceDefinition::Kv {
                    history, ttl_ms, ..
                } => ("kv", *history, *ttl_ms),
                ResourceDefinition::Store { ttl_ms, .. } => ("store", 0, *ttl_ms),
                ResourceDefinition::Consumer { .. } => ("consumer", 0, 0),
                ResourceDefinition::Job { .. } => ("job", 0, 0),
                ResourceDefinition::State { .. } => ("state", 0, 0),
            };
            let name = name.to_string();
            let resource_id = digest(&("deployment", deployment, &participant, kind, &name))?;
            let state = if let ResourceDefinition::State {
                schema,
                version,
                accepts,
                ..
            } = declaration
            {
                Some(super::state::StateDeclaration {
                    resource_id: resource_id.clone(),
                    resource_name: name.clone(),
                    schema: trellis_idl::json_schema(&graph, schema)
                        .map_err(|error| AuthError::Invalid(error.to_string()))?,
                    representation_version: *version,
                    accepted_versions: accepts
                        .iter()
                        .map(|old| {
                            Ok((
                                old.version,
                                trellis_idl::json_schema(&graph, &old.ty)
                                    .map_err(|error| AuthError::Invalid(error.to_string()))?,
                            ))
                        })
                        .collect::<Result<_, AuthError>>()?,
                })
            } else {
                None
            };
            let consumer = if let ResourceDefinition::Consumer {
                events,
                concurrency,
                replay,
                retry,
                ..
            } = declaration
            {
                Some(EventConsumerResourceBinding {
                    resource_id: resource_id.clone(),
                    stream: crate::resources::EVENT_STREAM.into(),
                    consumer_name: String::new(),
                    filter_subjects: events
                        .iter()
                        .map(|(api, event)| {
                            trellis_protocol::derive_event_subject(api.as_str(), event)
                                .map_err(|error| AuthError::Invalid(error.to_string()))
                        })
                        .collect::<Result<_, _>>()?,
                    concurrency: *concurrency,
                    replay: match replay {
                        trellis_idl::Replay::New => trellis_rs::service::EventConsumerReplay::New,
                        trellis_idl::Replay::All => trellis_rs::service::EventConsumerReplay::All,
                    },
                    ack_wait_ms: 30_000,
                    max_deliver: retry.as_ref().map_or(5, |retry| i64::from(retry.attempts)),
                    backoff_ms: retry
                        .as_ref()
                        .map(|retry| {
                            retry
                                .backoff_ms
                                .iter()
                                .map(|value| i64::try_from(*value).map_err(|_| AuthError::Denied))
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()?
                        .unwrap_or_else(|| vec![5_000, 30_000, 120_000, 600_000]),
                    replay_binding: trellis_rs::service::EventConsumerReplayBinding {
                        stream: trellis_events_runtime::REPLAY_STREAM.into(),
                        consumer_name: String::new(),
                    },
                })
            } else {
                None
            };
            let job = if let ResourceDefinition::Job {
                payload,
                result,
                update,
                deadline_ms,
                retry,
                key_concurrency,
                ..
            } = declaration
            {
                use trellis_rs::jobs::bindings::{
                    JobKeyConcurrencyBinding, JobKeyStalePolicy, JobQueueDepthBinding,
                    JobQueueWhenFull,
                };
                let namespace = digest(&("jobs", deployment))?;
                Some(trellis_rs::service::JobsQueueResourceBinding {
                    queue_type: name.clone(),
                    publish_prefix: format!("trellis.jobs.{namespace}.{name}"),
                    updates_prefix: update
                        .as_ref()
                        .map(|_| format!("trellis.jobs_updates.{namespace}.{name}")),
                    work_subject: format!("trellis.work.{namespace}.{name}"),
                    consumer_name: String::new(),
                    payload: trellis_rs::service::JobsSchemaRef {
                        schema: payload.id.to_string(),
                    },
                    result: result
                        .as_ref()
                        .map(|ty| trellis_rs::service::JobsSchemaRef {
                            schema: ty.id.to_string(),
                        }),
                    update: update
                        .as_ref()
                        .map(|ty| trellis_rs::service::JobsSchemaRef {
                            schema: ty.id.to_string(),
                        }),
                    max_deliver: retry.as_ref().map_or(5, |retry| i64::from(retry.attempts)),
                    backoff_ms: retry
                        .as_ref()
                        .map(|retry| {
                            retry
                                .backoff_ms
                                .iter()
                                .map(|value| i64::try_from(*value).map_err(|_| AuthError::Denied))
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()?
                        .unwrap_or_else(|| vec![5_000, 30_000, 120_000, 600_000]),
                    ack_wait_ms: 30_000,
                    default_deadline_ms: deadline_ms
                        .map(|value| i64::try_from(value).map_err(|_| AuthError::Denied))
                        .transpose()?,
                    key_concurrency: key_concurrency
                        .as_ref()
                        .map(|key| JobKeyConcurrencyBinding {
                            key: vec![format!("/{}", key.path.join("/"))],
                            max_active: 1,
                            heartbeat_interval_ms: 10_000,
                            heartbeat_ttl_ms: 30_000,
                            stale_policy: JobKeyStalePolicy::Block,
                        }),
                    queue: key_concurrency.as_ref().map(|key| JobQueueDepthBinding {
                        max_queued_per_key: match key.policy {
                            trellis_idl::KeyConcurrencyPolicy::Queue => u64::from(u32::MAX),
                            trellis_idl::KeyConcurrencyPolicy::Reject => 0,
                            trellis_idl::KeyConcurrencyPolicy::Supersede => 1,
                        },
                        when_full: match key.policy {
                            trellis_idl::KeyConcurrencyPolicy::Supersede => {
                                JobQueueWhenFull::ReplaceOldest
                            }
                            _ => JobQueueWhenFull::Reject,
                        },
                    }),
                })
            } else {
                None
            };
            kv.insert(
                name.clone(),
                NativeAllocation {
                    kind: kind.into(),
                    resource_id,
                    allocation_id: super::sqlite::id(),
                    history: i64::try_from(history).map_err(|_| AuthError::Denied)?,
                    ttl_ms: i64::try_from(ttl_ms).map_err(|_| AuthError::Denied)?,
                    consumer,
                    job,
                    state,
                },
            );
        }
        let mut materialized = BTreeSet::new();
        let mut previous_jobs = connection.prepare(
            "SELECT actual_json FROM auth_resource_binding_evidence \
            WHERE owner_kind='deployment' \
            AND owner_id=?1 AND resource_kind='jobQueue'",
        )?;
        let mut key_bucket_materialized = false;
        for binding in previous_jobs.query_map([deployment], |row| row.get::<_, String>(0))? {
            let binding: trellis_rs::service::JobsQueueResourceBinding = decode(&binding?)?;
            key_bucket_materialized |= binding.key_concurrency.is_some();
        }
        for (name, allocation) in &mut kv {
            if let Some(declaration) = &allocation.state {
                let mut history = connection.prepare(
                    "SELECT commitment_json FROM auth_resource_history WHERE resource_id=?1",
                )?;
                for previous in
                    history.query_map([&allocation.resource_id], |row| row.get::<_, String>(0))?
                {
                    let previous: super::state::StateDeclaration = decode(&previous?)?;
                    for (version, schema) in
                        std::iter::once((&declaration.representation_version, &declaration.schema))
                            .chain(&declaration.accepted_versions)
                    {
                        let old = if *version == previous.representation_version {
                            Some(&previous.schema)
                        } else {
                            previous.accepted_versions.get(version)
                        };
                        if old.is_some_and(|old| old != schema) {
                            return Err(AuthError::Invalid(
                                "State representation versions are immutable".into(),
                            ));
                        }
                    }
                }
            }
            let current: Option<(String, String, bool)> = connection
                .query_row(
                    "SELECT state,allocation_id,actual_json IS NOT NULL \
                    FROM auth_resources \
                    WHERE resource_id=?1",
                    [&allocation.resource_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((state, incarnation, previously_materialized)) = current {
                if state == "destroying" {
                    return Err(AuthError::Conflict);
                }
                allocation.allocation_id = incarnation;
                if previously_materialized {
                    materialized.insert(allocation.resource_id.clone());
                }
            }
            if let Some(consumer) = &mut allocation.consumer {
                consumer.consumer_name = format!(
                    "TRELLIS_RESOURCE_{}_{}",
                    allocation.resource_id, allocation.allocation_id
                );
                consumer.replay_binding.consumer_name =
                    format!("{}_replay", consumer.consumer_name);
            }
            if let Some(job) = &mut allocation.job {
                job.consumer_name = format!(
                    "TRELLIS_RESOURCE_{}_{}",
                    allocation.resource_id, allocation.allocation_id
                );
                let namespace = digest(&("jobs", deployment))?;
                let queue_token = format!("{name}_{}", allocation.allocation_id);
                job.publish_prefix = format!("trellis.jobs.{namespace}.{queue_token}");
                job.work_subject = format!("trellis.work.{namespace}.{queue_token}");
                if job.updates_prefix.is_some() {
                    job.updates_prefix =
                        Some(format!("trellis.jobs_updates.{namespace}.{queue_token}"));
                }
            }
            connection.execute(
                "INSERT INTO auth_resources(resource_id,allocation_id,owner_kind,\
                owner_id,participant_id,kind,local_name,commitment_json,state,\
                binding_revision,revision,created_at,updated_at,physical_id) \
                VALUES(?1,?9,'deployment',?2,?3,?10,?4,?5,'pending',?6,1,?7,?7,?8) \
                ON CONFLICT(resource_id) \
                DO UPDATE SET state='pending',updated_at=excluded.updated_at",
                params![
                    allocation.resource_id,
                    deployment,
                    participant,
                    name,
                    json(allocation)?,
                    binding_revision,
                    now,
                    format!(
                        "TRELLIS_RESOURCE_{}_{}",
                        allocation.resource_id, allocation.allocation_id
                    ),
                    allocation.allocation_id,
                    allocation.kind
                ],
            )?;
        }
        Ok(ResourcePlan {
            principal,
            deployment: deployment.into(),
            participant,
            installed_revision,
            binding_revision,
            kv,
            materialized,
            key_bucket_materialized,
        })
    })?;
    let jetstream = async_nats::jetstream::new(operational.clone());
    // Empty projections are valid only after inspecting the installed declaration set.
    let mut bindings = ServiceResourceBindings::default();
    let allocation_result: Result<(),AuthError> = async {
    super::framework_storage::materialize_operations(store, operational, deployment, now).await?;
    for (name, allocation) in &plan.kv {
        if allocation.state.is_some() {
            bindings
                .state
                .insert(name.clone(), allocation.resource_id.clone());
            continue;
        }
        let bucket = format!(
            "TRELLIS_RESOURCE_{}_{}",
            allocation.resource_id, allocation.allocation_id
        );
        let marker = format!(
            "trellis.resource.v1:{}:{}",
            allocation.resource_id, allocation.allocation_id
        );
        if let Some(binding) = &allocation.job {
            let namespace = digest(&("jobs", deployment))?;
            let stream = jetstream
                .get_stream(crate::resources::JOBS_WORK_STREAM)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?;
            let backoff: Vec<_> = binding
                .backoff_ms
                .iter()
                .map(|ms| Duration::from_millis(*ms as u64))
                .collect();
            let ack_wait = backoff
                .first()
                .copied()
                .unwrap_or(Duration::from_millis(binding.ack_wait_ms as u64));
            match stream.consumer_info(&binding.consumer_name).await {
                Ok(info) => {
                    let config = &info.config;
                    if config.description.as_deref() != Some(marker.as_str())
                        || config.filter_subject != binding.work_subject
                        || config.ack_policy != async_nats::jetstream::consumer::AckPolicy::Explicit
                        || config.max_deliver != -1
                        || config.ack_wait != ack_wait
                        || config.backoff != backoff
                    {
                        return Err(AuthError::Conflict);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        async_nats::jetstream::context::ConsumerInfoErrorKind::NotFound
                    ) =>
                {
                    if plan.materialized.contains(&allocation.resource_id) {
                        return Err(AuthError::Broker(format!(
                            "previously materialized Job consumer '{}' is missing",
                            binding.consumer_name
                        )));
                    }
                    stream
                        .create_consumer_strict(async_nats::jetstream::consumer::pull::Config {
                            durable_name: Some(binding.consumer_name.clone()),
                            description: Some(marker.clone()),
                            filter_subject: binding.work_subject.clone(),
                            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                            ack_wait,
                            max_deliver: -1,
                            backoff,
                            ..Default::default()
                        })
                        .await
                        .map_err(|error| AuthError::Broker(error.to_string()))?;
                }
                Err(error) => return Err(AuthError::Broker(error.to_string())),
            }
            if binding.key_concurrency.is_some() {
                let key_bucket = format!("JOBS_KEYS_{namespace}");
                let key_marker = format!("trellis.jobs.keys.v1:{deployment}");
                let stream_name = format!("KV_{key_bucket}");
                match jetstream.get_stream(&stream_name).await {
                    Ok(mut stream) => {
                        let config = &stream
                            .info()
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?
                            .config;
                        if config.description.as_deref() != Some(key_marker.as_str())
                            || config.storage != async_nats::jetstream::stream::StorageType::File
                            || config.max_age != Duration::ZERO
                            || config.subjects != vec![format!("$KV.{key_bucket}.>")]
                        {
                            return Err(AuthError::Conflict);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            GetStreamErrorKind::JetStream(error)
                                if error.error_code()==ErrorCode::STREAM_NOT_FOUND
                        ) =>
                    {
                        if plan.key_bucket_materialized {
                            return Err(AuthError::Broker(format!(
                                "previously materialized Job key bucket '{key_bucket}' is missing"
                            )));
                        }
                        jetstream
                            .create_key_value(async_nats::jetstream::kv::Config {
                                bucket: key_bucket,
                                description: key_marker,
                                history: 1,
                                storage: async_nats::jetstream::stream::StorageType::File,
                                ..Default::default()
                            })
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
                    }
                    Err(error) => return Err(AuthError::Broker(error.to_string())),
                }
            }
            bindings
                .jobs
                .get_or_insert_with(|| trellis_rs::service::JobsResourceBinding {
                    service_name: plan.participant.clone(),
                    namespace,
                    work_stream: Some(crate::resources::JOBS_WORK_STREAM.into()),
                    queues: BTreeMap::new(),
                })
                .queues
                .insert(name.clone(), binding.clone());
            continue;
        }
        if let Some(binding) = &allocation.consumer {
            for (stream_name, consumer_name, filters, replay) in [
                (
                    &binding.stream,
                    &binding.consumer_name,
                    binding.filter_subjects.clone(),
                    match binding.replay {
                        trellis_rs::service::EventConsumerReplay::New => {
                            async_nats::jetstream::consumer::DeliverPolicy::New
                        }
                        _ => async_nats::jetstream::consumer::DeliverPolicy::All,
                    },
                ),
                (
                    &binding.replay_binding.stream,
                    &binding.replay_binding.consumer_name,
                    vec![trellis_events_runtime::dead_letters::replay_filter_subject(
                        &allocation.resource_id,
                    )],
                    async_nats::jetstream::consumer::DeliverPolicy::All,
                ),
            ] {
                let stream = jetstream
                    .get_stream(stream_name)
                    .await
                    .map_err(|error| AuthError::Broker(error.to_string()))?;
                match stream.consumer_info(consumer_name).await {
                    Ok(info) => {
                        let config = &info.config;
                        // Existing delivery position and unacknowledged messages belong
                        // to this allocation. Never recreate or widen it on reapply.
                        if config.description.as_deref() != Some(marker.as_str())
                            || config.filter_subjects.iter().collect::<BTreeSet<_>>()
                                != filters.iter().collect()
                            || config.deliver_policy != replay
                            || config.ack_policy
                                != async_nats::jetstream::consumer::AckPolicy::Explicit
                            || config.max_deliver != binding.max_deliver
                        {
                            return Err(AuthError::Conflict);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            async_nats::jetstream::context::ConsumerInfoErrorKind::NotFound
                        ) =>
                    {
                        if plan.materialized.contains(&allocation.resource_id) {
                            return Err(AuthError::Broker(format!("previously materialized Event consumer '{consumer_name}' is missing")));
                        }
                        stream
                            .create_consumer_strict(async_nats::jetstream::consumer::pull::Config {
                                durable_name: Some(consumer_name.clone()),
                                description: Some(marker.clone()),
                                filter_subjects: filters,
                                deliver_policy: replay,
                                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                                ack_wait: Duration::from_millis(binding.ack_wait_ms as u64),
                                max_deliver: binding.max_deliver,
                                ..Default::default()
                            })
                            .await
                            .map_err(|error| AuthError::Broker(error.to_string()))?;
                    }
                    Err(error) => return Err(AuthError::Broker(error.to_string())),
                }
            }
            bindings
                .event_consumers
                .insert(name.clone(), binding.clone());
            continue;
        }
        if allocation.kind == "store" {
            let stream_name = format!("OBJ_{bucket}");
            match jetstream.get_stream(&stream_name).await {
                Ok(_) => (),
                Err(error)
                    if matches!(
                        error.kind(),
                        GetStreamErrorKind::JetStream(error)
                            if error.error_code()==ErrorCode::STREAM_NOT_FOUND
                    ) =>
                {
                    if plan.materialized.contains(&allocation.resource_id) {
                        return Err(AuthError::Broker(format!(
                            "previously materialized object store '{bucket}' is missing"
                        )));
                    }
                    jetstream
                        .create_object_store(async_nats::jetstream::object_store::Config {
                            bucket: bucket.clone(),
                            description: Some(marker.clone()),
                            max_age: Duration::from_millis(allocation.ttl_ms as u64),
                            storage: async_nats::jetstream::stream::StorageType::File,
                            ..Default::default()
                        })
                        .await
                        .map_err(|error| AuthError::Broker(error.to_string()))?;
                }
                Err(error) => return Err(AuthError::Broker(error.to_string())),
            }
            let mut stream = jetstream
                .get_stream(&stream_name)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?;
            let mut config = stream
                .info()
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?
                .config
                .clone();
            if config.description.as_deref() != Some(marker.as_str())
                || config.storage != async_nats::jetstream::stream::StorageType::File
                || config.subjects.iter().collect::<BTreeSet<_>>()
                    != [format!("$O.{bucket}.C.>"), format!("$O.{bucket}.M.>")]
                        .iter()
                        .collect()
            {
                return Err(AuthError::Conflict);
            }
            let ttl = Duration::from_millis(allocation.ttl_ms as u64);
            if ttl != Duration::ZERO && (config.max_age == Duration::ZERO || ttl < config.max_age) {
                return Err(AuthError::Invalid("potential_data_loss".into()));
            }
            if ttl != config.max_age {
                config.max_age = ttl;
                jetstream
                    .update_stream(config)
                    .await
                    .map_err(|error| AuthError::Broker(error.to_string()))?;
                config = stream
                    .info()
                    .await
                    .map_err(|error| AuthError::Broker(error.to_string()))?
                    .config
                    .clone();
            }
            bindings.store.insert(
                name.clone(),
                StoreResourceBinding {
                    name: bucket,
                    ttl_ms: i64::try_from(config.max_age.as_millis())
                        .map_err(|_| AuthError::Unavailable)?,
                    max_total_bytes: (config.max_bytes > 0).then_some(config.max_bytes),
                    max_object_bytes: None,
                },
            );
            continue;
        }
        let resource = match jetstream.get_stream(format!("KV_{bucket}")).await {
            Ok(_) => jetstream
                .get_key_value(&bucket)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?,
            Err(error)
                if matches!(
                    error.kind(),
                    GetStreamErrorKind::JetStream(error)
                        if error.error_code()==ErrorCode::STREAM_NOT_FOUND
                ) =>
            {
                if plan.materialized.contains(&allocation.resource_id) {
                    return Err(AuthError::Broker(format!(
                        "previously materialized KV bucket '{bucket}' is missing"
                    )));
                }
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
        if allocation.history < status.history()
            || (desired_ttl != Duration::ZERO
                && (status.max_age() == Duration::ZERO || desired_ttl < status.max_age()))
        {
            return Err(AuthError::Invalid("potential_data_loss".into()));
        }
        if allocation.history != status.history() || desired_ttl != status.max_age() {
            let mut config = status.info.config.clone();
            config.max_messages_per_subject = allocation.history;
            config.max_age = desired_ttl;
            jetstream
                .update_stream(config)
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?;
            status = resource
                .status()
                .await
                .map_err(|error| AuthError::Broker(error.to_string()))?;
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
            let mut failed = false;
            for allocation in plan.kv.values() {
                if connection.execute(
                    "UPDATE auth_resources \
                    SET state='failed',revision=revision+1,updated_at=?3 \
                    WHERE resource_id=?1 \
                    AND allocation_id=?2 \
                    AND state='pending'",
                    params![allocation.resource_id, allocation.allocation_id, now],
                )? == 1
                {
                    failed = true;
                    connection.execute(
                        "UPDATE auth_resource_binding_evidence \
                        SET state='unavailable' \
                        WHERE binding_id=?1",
                        params![allocation.resource_id],
                    )?;
                }
            }
            if failed {
                SqliteAuthorizationStore::policy_changed(
                    connection,
                    &super::EnforcementScope::Deployment(plan.deployment.clone()),
                    now,
                )?;
            }
            Ok(())
        })?;
        return Err(error);
    }
    store.transaction(|connection| {
        let current: Option<(u64, u64)> = connection
            .query_row(
                "SELECT b.installed_revision,b.revision \
                FROM auth_deployment_bindings b \
                JOIN auth_deployments d USING(deployment_id) \
                JOIN auth_principals p ON p.principal_id=?2 \
                WHERE b.deployment_id=?1 \
                AND d.state='active' \
                AND p.state='active'",
                params![plan.deployment, plan.principal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if current != Some((plan.installed_revision, plan.binding_revision)) {
            return Err(AuthError::Conflict);
        }
        for (name, allocation) in &plan.kv {
            let (physical, actual, evidence_kind) = if let Some(declaration) = &allocation.state {
                (
                    format!("{}.{}", allocation.resource_id, allocation.allocation_id),
                    json(declaration)?,
                    "state",
                )
            } else if let Some(binding) = &allocation.job {
                (
                    json(&serde_json::json!({
                        "namespace":bindings.jobs.as_ref().ok_or(AuthError::Unavailable)?.namespace,
                        "work_stream":crate::resources::JOBS_WORK_STREAM,
                        "consumer":binding.consumer_name,
                        "publish_prefix":binding.publish_prefix,
                        "work_subject":binding.work_subject
                    }))?,
                    json(binding)?,
                    "jobQueue",
                )
            } else if let Some(binding) = &allocation.consumer {
                (
                    json(&serde_json::json!({
                        "stream":binding.stream,
                        "consumer":binding.consumer_name,
                        "filter_subjects":binding.filter_subjects,
                        "replay_consumer":binding.replay_binding.consumer_name
                    }))?,
                    json(binding)?,
                    "eventConsumer",
                )
            } else if allocation.kind == "store" {
                let binding = &bindings.store[name];
                (binding.name.clone(), json(binding)?, "store")
            } else {
                let binding = &bindings.kv[name];
                (binding.bucket.clone(), json(binding)?, "kv")
            };
            let commitment = if let Some(declaration) = &allocation.state {
                json(declaration)?
            } else {
                json(allocation)?
            };
            if connection.execute(
                "UPDATE auth_resources \
                SET commitment_json=?2,physical_id=?3,actual_json=?4,state='ready',\
                binding_revision=?5,revision=revision+1,updated_at=?6 \
                WHERE resource_id=?1 \
                AND allocation_id=?7 \
                AND state='pending'",
                params![
                    allocation.resource_id,
                    commitment,
                    physical,
                    actual,
                    plan.binding_revision,
                    now,
                    allocation.allocation_id
                ],
            )? != 1
            {
                return Err(AuthError::Conflict);
            }
            connection.execute(
                "INSERT INTO auth_resource_history SELECT resource_id,revision,\
                commitment_json,actual_json,state,?2 \
                FROM auth_resources \
                WHERE resource_id=?1",
                params![allocation.resource_id, now],
            )?;
            connection.execute(
                "UPDATE auth_resource_binding_evidence \
                SET state='unavailable' \
                WHERE binding_id=?1",
                [&allocation.resource_id],
            )?;
            connection.execute(
                "INSERT INTO auth_resource_binding_evidence(owner_kind,owner_id,\
                participant_id,installed_revision,resource_kind,local_name,binding_id,\
                provider_identity,actual_json,state,materialized_at) \
                VALUES('deployment',?1,?2,?3,?9,?4,?5,?6,?7,'available',?8) \
                ON CONFLICT(owner_kind,owner_id,participant_id,installed_revision,\
                resource_kind,local_name) \
                DO UPDATE SET provider_identity=excluded.provider_identity,\
                actual_json=excluded.actual_json,state='available',\
                materialized_at=excluded.materialized_at",
                params![
                    plan.deployment,
                    plan.participant,
                    plan.installed_revision,
                    name,
                    allocation.resource_id,
                    physical,
                    actual,
                    now,
                    evidence_kind
                ],
            )?;
        }
        connection.execute(
            "UPDATE auth_deployment_bindings \
            SET resource_commitments_json=?1 \
            WHERE deployment_id=?2",
            params![json(&bindings)?, plan.deployment],
        )?;
        Ok(bindings.clone())
    })
}
