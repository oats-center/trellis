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

#[cfg(test)]
mod tests {
    use std::fs;

    #[test]
    fn exposes_core_facade_modules() {
        let _options =
            crate::service::ServiceConnectOptions::new("http://localhost:8080", "identity-seed");
        let _state = crate::jobs::JobState::Pending;
    }

    #[test]
    fn low_level_workspace_crates_are_not_publishable_packages() {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let crates_dir = crate_dir
            .parent()
            .expect("trellis crate should live under crates");
        for entry in fs::read_dir(crates_dir).expect("workspace crates should be readable") {
            let entry = entry.expect("crate directory should be readable");
            if entry.file_name() == "trellis" || entry.file_name() == "protocol" {
                continue;
            }
            let manifest = entry.path().join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let contents =
                fs::read_to_string(&manifest).expect("internal crate manifest should be readable");
            assert!(
                contents.contains("publish = false"),
                "{} must stay non-publishable",
                manifest.display()
            );
        }
    }

    #[test]
    fn trellis_does_not_depend_on_generated_trellis_owned_sdk_packages() {
        let manifest =
            fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("trellis manifest should be readable");
        for package in [
            "trellis-sdk-auth",
            "trellis-sdk-core",
            "trellis-sdk-health",
            "trellis-sdk-jobs",
            "trellis-sdk-state",
        ] {
            assert!(
                !manifest.contains(package),
                "{package} must remain an internal runtime projection, not a trellis dependency"
            );
        }
    }

    #[test]
    fn trellis_does_not_depend_on_old_internal_package_identities() {
        let manifest =
            fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("trellis manifest should be readable");
        for package in [
            "trellis-auth",
            "trellis-client",
            "trellis-jobs",
            "trellis-service",
            "trellis-service-runtime",
        ] {
            assert!(
                !manifest.contains(package),
                "{package} must be implemented as a trellis module, not a trellis dependency"
            );
        }
    }
}
