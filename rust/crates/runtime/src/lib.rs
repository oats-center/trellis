#![deny(missing_docs)]

//! Rust runtime entrypoint support for Trellis.
//!
//! Runtime modes select platform, Jobs, Health, or Event Log ownership groups;
//! `all` acquires every group. With NATS leases and SQLite enabled, [`run`]
//! acquires all selected singleton leases before opening owner-controlled stores
//! or starting subsystems. Ownership loss is fatal and does not trigger standby
//! or reacquisition. Shutdown is bounded across subsystem stop, HTTP drain,
//! lease release, and NATS flush.

/// Runtime configuration loading, defaults, and validation.
pub mod config;
/// Runtime mode parsing and subsystem selection.
pub mod mode;
/// Minimal HTTP readiness and version server.
pub mod server;
/// Cooperative runtime shutdown primitives.
pub mod shutdown;

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
/// Built-in Events subsystem.
pub mod events;
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
/// Health subsystem scaffold.
pub mod health;
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
/// Jobs subsystem scaffold.
pub mod jobs;
#[cfg(feature = "nats-leases")]
/// NATS-backed lease primitives.
pub mod leases;
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
mod ownership;
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
/// Platform subsystem scaffold and bootstrap services.
pub mod platform;
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
mod resources;
#[cfg(feature = "sqlite-storage")]
/// SQLite storage for built-in runtime subsystems.
pub mod storage;
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
/// Runtime supervisor startup and subsystem lifecycle orchestration.
pub mod supervisor;
mod telemetry;

pub use config::{
    AuthConfig, AuthorizationConfig, ClientConfig, ConfigError, HttpConfig, LeasesConfig,
    LocalIdentityConfig, NatsAuthCalloutConfig, NatsConfig, NatsRuntimeConfig, OAuthConfig,
    OAuthProviderConfig, PlatformTtlConfig, ResolvedLeasesConfig, ResolvedNatsAuthCalloutConfig,
    ResolvedRuntimeNatsConfig, RuntimeConfig, RuntimePathDefaults, RuntimePathsConfig,
    SqliteStorageConfig, StorageBackend, StorageConfig, SubsystemConfig,
};
pub use mode::{RuntimeMode, RuntimeModeParseError, SubsystemName};
pub use server::{build_version_info, run_http_server, ServerError, VersionInfo};

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
pub use supervisor::{run, RuntimeError, RuntimeOptions};

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
pub use supervisor::{check, NatsEndpointOverride, RuntimeCheckReport};
