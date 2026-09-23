//! Live observation sessions shared by Live and Operation observation.
//!
//! This module owns the per-connection session runtime: retained authority
//! guards, prepared provider and consumer sessions, the serialized control and
//! publication paths, and the public owned subscription handle. It is
//! crate-private assembly; the public surface is re-exported from the crate
//! root.
#![allow(
    dead_code,
    reason = "liveness, stall, and Operation observation still consume remaining engine items"
)]

pub(crate) mod authority;
pub(crate) mod client_open;
pub(crate) mod deadlines;
pub(crate) mod manager;

pub use manager::LiveSessionManager;
pub(crate) mod provider;
pub(crate) mod provider_engine;
pub(crate) mod subscription;
pub(crate) mod telemetry;
pub(crate) mod types;

pub use types::{
    CloseCleanupState, CloseRemoteState, LiveCancellation, LiveCloseReceipt, LiveEnd,
    LiveEndReason, LiveErrorCode, LiveStreamError,
};
