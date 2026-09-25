//! Curated public Rust facade for Trellis clients, services, contracts, auth, and jobs.
//!
//! This crate is the normal Rust authoring entrypoint. It re-exports stable,
//! commonly used runtime types without exposing low-level service loops,
//! bootstrap hosts, or generated descriptor internals.
//!
//! Generated SDK crates and participant facades include a package-local
//! `TRELLIS.md` for AI-agent use. Participant facades are the connection
//! boundary and expose generated caller methods through `.client()`, service
//! resources through `.service()`, and provider registration through
//! `.handle()`.
//!
//! Prepared event and outbox/inbox support lives under [`client`]:
//! `PreparedTrellisEvent`, `prepare_event::<Descriptor>(...)`,
//! `publish_prepared`, `dispatch_outbox_once`, `OutboxStore`, `InboxStore`,
//! `SqliteOutboxStore`, `SqliteInboxStore`, `PostgresOutboxStore`, and
//! `PostgresInboxStore`.
//!
//! # Authoring model
//!
//! Generated participant facades are the normal connection boundary. Their
//! generated `.client()` methods call typed RPCs, invoke operations and signals,
//! publish or subscribe to events, and access contract state without exposing
//! subjects. Service facades connect with [`service::ServiceConnectOptions`],
//! register generated RPC and operation handlers, publish typed events, process
//! private Jobs queues, and access resolved KV and object-store handles.
//!
//! Connection and request failures retain typed authentication, transport,
//! generated declared-RPC, protocol, and bootstrap errors. Callers should retry only
//! errors documented as transient. Native bootstrap resolves the server-owned
//! assignment and current grants from a provisioned seed.
//!
//! ```no_run
//! use trellis_rs::service::ServiceConnectOptions;
//!
//! let _options = ServiceConnectOptions::new(
//!     "http://localhost:3000",
//!     "base64url-identity-seed",
//! )
//! .with_name("documents-worker")
//! .with_timeout_ms(10_000);
//! ```

#[doc(hidden)]
pub mod client;

pub(crate) mod live;

pub use live::subscription::LiveSubscription;
pub use live::LiveSessionManager;
pub use live::{
    CloseCleanupState, CloseRemoteState, LiveCancellation, LiveCloseReceipt, LiveEnd,
    LiveEndReason, LiveErrorCode, LiveStreamError,
};

#[doc(hidden)]
pub mod generated;

/// High-level service runtime and service-authoring support types.
pub mod service;

#[doc(hidden)]
pub mod auth;

#[doc(hidden)]
pub mod jobs;

/// Reusable OpenTelemetry ownership and instrumentation.
pub mod telemetry;

extern crate self as trellis_rs;
