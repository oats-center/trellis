//! Built-in Jobs admin subsystem.

use std::sync::Arc;

use trellis_jobs_runtime::{
    jobs_admin_resources, start_jobs_projector, start_worker_presence_projector,
    JobResourceResolver, JobsAdminResources, JobsProjectorHandle, JobsQuery, SqliteJobsStore,
    WorkerPresenceProjectorHandle,
};
use trellis_rs::service::{internal::run_builtin_authenticated_router, ServerError};

use crate::shutdown::StopHandle;
use crate::supervisor::{RuntimeContext, RuntimeError, SubsystemHandle};
use crate::{StorageBackend, SubsystemName};

const JOBS_API_ID: &str = "trellis.jobs@v1";
mod resolver;

fn runtime_error(error: ServerError) -> RuntimeError {
    RuntimeError::Nats(error.to_string())
}

struct RuntimeLoops {
    projector: JobsProjectorHandle,
    worker_presence: WorkerPresenceProjectorHandle,
}

impl RuntimeLoops {
    async fn start(
        jobs_runtime: trellis_rs::jobs::JobsRuntime,
        resources: &JobsAdminResources,
        store: SqliteJobsStore,
    ) -> Result<Self, RuntimeError> {
        let projector = start_jobs_projector(
            jobs_runtime.clone(),
            store.clone(),
            resources.jobs_stream.clone(),
        )
        .await
        .map_err(runtime_error)?;
        let worker_presence = match start_worker_presence_projector(
            jobs_runtime,
            resources.jobs_stream.clone(),
            store,
        )
        .await
        {
            Ok(handle) => handle,
            Err(error) => {
                projector.stop().await;
                return Err(runtime_error(error));
            }
        };
        Ok(Self {
            projector,
            worker_presence,
        })
    }

    async fn stop(self) {
        self.projector.stop().await;
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
    let StorageBackend::Sqlite(storage) = context
        .config
        .jobs_storage_backend()
        .map_err(RuntimeError::Config)?;
    let store = SqliteJobsStore::open(&storage.path)
        .map_err(|error| RuntimeError::Nats(format!("failed to open Jobs SQLite: {error}")))?;
    let jobs_runtime = trellis_rs::jobs::JobsRuntime::from_nats(context.trellis_nats.clone());
    let resources = jobs_admin_resources();
    let evidence = crate::resource_evidence::ResourceEvidenceReader::new(
        context
            .live_providers
            .receiver(crate::platform::LiveProviderRole::Jobs),
        stop.clone(),
    );
    let resolver: Arc<dyn JobResourceResolver> = Arc::new(resolver::AuthJobResourceResolver {
        evidence: evidence.clone(),
        nats: context.trellis_nats.clone(),
        jobs_stream: resources.jobs_stream.clone(),
    });
    let query = JobsQuery::with_store(jobs_runtime.clone(), store.clone(), Arc::clone(&resolver));
    let mut router = trellis_jobs_runtime::build_router_with_query(query);
    trellis_jobs_runtime::register_jobs_watch_live(
        &mut router,
        jobs_runtime.clone(),
        resources.jobs_stream.clone(),
    );
    let sampler_store = store.clone();
    let loops = RuntimeLoops::start(jobs_runtime, &resources, store).await?;
    let sampler_nats = context.trellis_nats.clone();
    let context_nats = context.trellis_nats.clone();
    let live_owner = context
        .live_providers
        .receiver(crate::platform::LiveProviderRole::Jobs);
    let join = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
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
            let mut router = router;
            history::register(&mut router, evidence, context_nats, owner.clone());
            router.set_live_owner(owner.clone());
            let validator = owner.runtime_validator(JOBS_API_ID);
            run_builtin_authenticated_router(owner, router, validator).await
        };
        tokio::pin!(api_loop);
        let result = {
            tokio::select! {
                biased;
                () = task_stop.stopped() => Ok(()),
                result = &mut api_loop => result.map_err(runtime_error),
                result = loops.wait_for_failure() => result,
            }
        };
        task_stop.stop();
        // Telemetry samplers own their tasks and never own business lifetime.
        samplers.stop().await;
        loops.stop().await;
        result
    }));

    Ok(SubsystemHandle {
        name: SubsystemName::Jobs,
        stop,
        join,
    })
}
mod history;
