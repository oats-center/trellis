//! Internal Jobs admin runtime for Trellis.
//!
//! This crate implements the admin-side loops and RPC hosting for the standard
//! `trellis.jobs@v1` Trellis API: SQLite-backed queries, stream projection, janitor
//! expiry, and advisory handling. Service-local job execution lives in
//! `trellis_rs::jobs` in the public `trellis` facade.

mod advisory;
mod janitor;
mod projector;
mod query;
mod resolver;
mod router;
pub mod storage;
pub mod telemetry;
mod watch;
pub mod worker_presence;

#[cfg(test)]
mod test_nats;

pub use advisory::{
    map_dead_event_from_advisory_job, start_advisory_loop, AdvisoryHandle, MappedDeadEvent,
    MaxDeliveriesAdvisory,
};
pub use janitor::{
    plan_expired_events, run_janitor_once, start_janitor_loop, JanitorError, JanitorHandle,
    JanitorRunStats, PlannedExpiredEvent,
};
pub use projector::{projector_consumer_name, start_jobs_projector, JobsProjectorHandle};
pub use query::{jobs_admin_resources, JobsAdminResources, JobsQuery, JobsQueryError};
pub use resolver::{
    JobKeySnapshot, JobResourceBinding, JobResourceResolver, SqliteJobResourceResolver,
};
pub use router::build_router_with_query;
pub use storage::{ListJobsFilter, SqliteJobsStore, SqliteJobsStoreError};
pub use watch::register_jobs_watch_live;
pub use worker_presence::{start_worker_presence_projector, WorkerPresenceProjectorHandle};
