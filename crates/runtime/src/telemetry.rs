//! Stable, low-cardinality OpenTelemetry instrumentation for runtime internals.
//!
//! The instrument implementation is owned by the Rust SDK's
//! [`trellis_rs::telemetry::instruments`] module so one instrument handle exists
//! per meter/name across the runtime and its embedding processes. This module
//! keeps the runtime's private import surface stable.

pub(crate) mod snapshots;
