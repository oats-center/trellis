//! Internal built-in Events runtime for Trellis.
//!
//! This crate hosts the read-only `trellis.events@v1` API and maintains the
//! SQLite projection used by the Trellis runtime.

mod consumers;
pub mod dead_letters;
mod management;
mod projector;
mod query;
mod router;
pub mod storage;
pub mod telemetry;
mod watch;
mod wire;

pub use dead_letters::{
    start_dead_letter_projector, start_exhaustion_advisory_loop, start_replay_dispatcher,
    ConsumerBinding, ConsumerBindingResolver, DeadLetterJournal, DeliveryReportError,
    DeliveryReporter, DLQ_STREAM, DLQ_SUBJECTS, REPLAY_STREAM, REPLAY_SUBJECTS,
};
pub use management::{
    secure_consumer_routes, EventsManagement, ManagementAction, ManagementAuthorizer,
};
pub use projector::{
    projector_consumer_name, start_events_projector, EventAuthorizationInput, EventVerifier,
    EventsProjectorHandle, EventsRuntime, VerifiedEventPublisher,
};
pub use query::{EventsQuery, EventsQueryError};
pub use router::build_router_with_query;
pub use storage::{EventsStore, EventsStoreError, ProjectedEvent};
pub use watch::register_events_watch_live;
