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

#[doc(hidden)]
pub mod generated;

/// High-level service runtime and service-authoring support types.
pub mod service;

#[doc(hidden)]
pub mod auth;

#[doc(hidden)]
pub mod jobs;

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
        for manifest in [
            "cli/Cargo.toml",
            "codegen-Cargo.toml",
            "codegen-ts/Cargo.toml",
            "bootstrap/Cargo.toml",
            "local-bootstrap/Cargo.toml",
            "protocol-wasm/Cargo.toml",
            "runtime-apis/Cargo.toml",
            "runtime/Cargo.toml",
            "events-runtime/Cargo.toml",
            "jobs-runtime/Cargo.toml",
            "trellis-test/Cargo.toml",
        ] {
            let contents = fs::read_to_string(crates_dir.join(manifest))
                .expect("internal crate manifest should be readable");
            assert!(
                contents.contains("publish = false"),
                "{manifest} must stay non-publishable"
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
