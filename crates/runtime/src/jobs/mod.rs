//! Built-in Jobs admin subsystem.

use std::sync::Arc;
use std::time::Duration;

use trellis_jobs_runtime::{
    jobs_admin_resources, start_advisory_loop, start_janitor_loop, start_jobs_projector,
    start_worker_presence_projector, AdvisoryHandle, JanitorHandle, JobResourceResolver,
    JobsAdminResources, JobsProjectorHandle, JobsQuery, SqliteJobResourceResolver, SqliteJobsStore,
    WorkerPresenceProjectorHandle,
};
use trellis_rs::service::{
    internal::run_builtin_authenticated_router, RequestValidator, ServerError,
};
use trellis_runtime_apis::apis::trellis_jobs_v1::{lives, rpc};

use crate::shutdown::StopHandle;
use crate::supervisor::{RuntimeContext, RuntimeError, SubsystemHandle};
use crate::{StorageBackend, SubsystemName};

/// Exact public Subjects the Jobs runtime serves on its provider connection.
///
/// The provider transport grants each implemented action exactly; wide
/// wildcards would exceed the materialized grants and are denied fail-closed.
const JOBS_SUBJECTS: &[&str] = &[
    rpc::Cancel::SUBJECT,
    rpc::DismissDLQ::SUBJECT,
    rpc::GetKey::SUBJECT,
    rpc::Inspect::SUBJECT,
    rpc::ListDLQ::SUBJECT,
    rpc::ListServices::SUBJECT,
    rpc::Metrics::SUBJECT,
    rpc::Query::SUBJECT,
    rpc::ReplayDLQ::SUBJECT,
    rpc::Retry::SUBJECT,
    rpc::Summary::SUBJECT,
    lives::Watch::SUBJECT,
];
const JOBS_API_ID: &str = "trellis.jobs@v1";
const DEFAULT_JANITOR_INTERVAL: Duration = Duration::from_secs(30);

fn janitor_interval() -> Duration {
    std::env::var("TRELLIS_JOBS_JANITOR_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_JANITOR_INTERVAL)
}

fn runtime_error(error: ServerError) -> RuntimeError {
    RuntimeError::Nats(error.to_string())
}

struct RuntimeLoops {
    advisory: AdvisoryHandle,
    janitor: JanitorHandle,
    projector: JobsProjectorHandle,
    worker_presence: WorkerPresenceProjectorHandle,
}

impl RuntimeLoops {
    async fn start(
        jobs_runtime: trellis_rs::jobs::JobsRuntime,
        resources: &JobsAdminResources,
        store: SqliteJobsStore,
        resolver: Arc<dyn JobResourceResolver>,
    ) -> Result<Self, RuntimeError> {
        let advisory = start_advisory_loop(
            jobs_runtime.clone(),
            store.clone(),
            resources.jobs_advisories_stream.clone(),
            resolver,
        )
        .await
        .map_err(runtime_error)?;
        let janitor =
            match start_janitor_loop(jobs_runtime.clone(), store.clone(), janitor_interval()).await
            {
                Ok(handle) => handle,
                Err(error) => {
                    advisory.stop().await;
                    return Err(runtime_error(error));
                }
            };
        let projector = match start_jobs_projector(
            jobs_runtime.clone(),
            store.clone(),
            resources.jobs_stream.clone(),
        )
        .await
        {
            Ok(handle) => handle,
            Err(error) => {
                let ((), ()) = tokio::join!(advisory.stop(), janitor.stop());
                return Err(runtime_error(error));
            }
        };
        let worker_presence = match start_worker_presence_projector(
            jobs_runtime,
            resources.jobs_stream.clone(),
            store,
        )
        .await
        {
            Ok(handle) => handle,
            Err(error) => {
                let ((), (), ()) = tokio::join!(advisory.stop(), janitor.stop(), projector.stop(),);
                return Err(runtime_error(error));
            }
        };
        Ok(Self {
            advisory,
            janitor,
            projector,
            worker_presence,
        })
    }

    async fn stop(self) {
        let ((), (), ()) = tokio::join!(
            self.projector.stop(),
            self.janitor.stop(),
            self.advisory.stop(),
        );
        self.worker_presence.stop().await;
    }

    async fn wait_for_failure(&mut self) -> Result<(), RuntimeError> {
        let (name, result) = tokio::select! {
            result = self.projector.wait() => {
                self.projector.discard_completed();
                ("projector", result)
            },
            result = self.worker_presence.wait() => {
                self.worker_presence.discard_completed();
                ("worker presence", result)
            },
            result = self.janitor.wait() => {
                self.janitor.discard_completed();
                ("janitor", result)
            },
            result = self.advisory.wait() => {
                self.advisory.discard_completed();
                ("advisory", result)
            },
        };
        match result {
            Ok(()) => Err(RuntimeError::Nats(format!(
                "jobs {name} loop exited unexpectedly"
            ))),
            Err(error) => Err(runtime_error(error)),
        }
    }
}

pub(crate) async fn start(context: &RuntimeContext) -> Result<SubsystemHandle, RuntimeError> {
    let owner = context.owner(crate::ownership::OwnerGroup::Jobs)?;
    let stop = StopHandle::new();
    let task_stop = stop.clone();
    let mut validator_join =
        crate::platform::auth::verifier::ensure_read_only(context, task_stop.clone()).await?;
    let StorageBackend::Sqlite(storage) = context
        .config
        .jobs_storage_backend()
        .map_err(RuntimeError::Config)?;
    let store = SqliteJobsStore::open(&storage.path)
        .map_err(|error| RuntimeError::Nats(format!("failed to open Jobs SQLite: {error}")))?;
    let jobs_runtime = trellis_rs::jobs::JobsRuntime::from_nats(context.trellis_nats.clone());
    let resources = jobs_admin_resources();
    let StorageBackend::Sqlite(platform_storage) = context
        .config
        .platform_storage_backend()
        .map_err(RuntimeError::Config)?;
    let resolver: Arc<dyn JobResourceResolver> = Arc::new(SqliteJobResourceResolver::new(
        platform_storage.path.clone(),
        context.trellis_nats.clone(),
        resources.jobs_stream.clone(),
    ));
    let query = JobsQuery::with_store(jobs_runtime.clone(), store.clone(), Arc::clone(&resolver));
    let mut router = trellis_jobs_runtime::build_router_with_query(query);
    trellis_jobs_runtime::register_jobs_watch_live(
        &mut router,
        jobs_runtime.clone(),
        resources.jobs_stream.clone(),
    );
    let validator: Arc<dyn RequestValidator> =
        Arc::new(context.platform_verifier.get().cloned().ok_or_else(|| {
            RuntimeError::Platform("local authorization verifier is not ready".to_owned())
        })?);
    let sampler_store = store.clone();
    let loops = RuntimeLoops::start(jobs_runtime, &resources, store, resolver).await?;
    let sampler_nats = context.trellis_nats.clone();
    let live_owner = context
        .live_providers
        .receiver(crate::platform::LiveProviderRole::Jobs);
    let join = tokio::spawn(async move {
        let _owner = owner;
        let mut loops = loops;
        let samplers = crate::telemetry::snapshots::SamplerOwner::start(vec![Box::pin(
            crate::telemetry::snapshots::run_jobs_sampler(
                sampler_store,
                sampler_nats,
                resources.jobs_stream.clone(),
                task_stop.clone(),
            ),
        )]);
        let api_stop = task_stop.clone();
        let api_loop = async move {
            let mut live_owner = live_owner;
            let Some(owner) = crate::platform::await_live_owner(&mut live_owner, &api_stop).await
            else {
                return Ok(());
            };
            let api_nats = owner.runtime_nats();
            let mut router = router;
            router.set_live_owner(owner);
            run_builtin_authenticated_router(
                api_nats,
                JOBS_API_ID,
                JOBS_SUBJECTS,
                router,
                validator,
            )
            .await
        };
        tokio::pin!(api_loop);
        let result = {
            let validator_exit = async {
                match validator_join.as_mut() {
                    Some(join) => match join.await {
                        Ok(Ok(())) => Err(RuntimeError::Platform(
                            "authorization validator cache exited unexpectedly".to_owned(),
                        )),
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(RuntimeError::Platform(format!(
                            "authorization validator cache task failed: {error}"
                        ))),
                    },
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(validator_exit);
            tokio::select! {
                biased;
                () = task_stop.stopped() => Ok(()),
                result = &mut api_loop => result.map_err(runtime_error),
                result = loops.wait_for_failure() => result,
                result = &mut validator_exit => result,
            }
        };
        task_stop.stop();
        // Telemetry samplers own their tasks and never own business lifetime.
        samplers.stop().await;
        loops.stop().await;
        if let Some(join) = validator_join {
            let _ = join.await;
        }
        result
    });

    Ok(SubsystemHandle {
        name: SubsystemName::Jobs,
        stop,
        join,
    })
}
