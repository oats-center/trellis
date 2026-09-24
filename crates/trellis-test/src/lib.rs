//! Isolated live Trellis runtimes for Rust integration tests.
//!
//! [`TrellisTestRuntime`] orchestrates normal production executables — the
//! released `trellis` CLI and `trellis-server` — out of process. It generates a
//! real bootstrap bundle, starts managed NATS, reserves four loopback ports
//! automatically, completes the real first-administrator bootstrap and login,
//! and exposes helpers that install participants and provision identities using
//! the generated administration API.
//!
//! The crate has no dependency on private Trellis implementation crates: its
//! only Trellis dependency is the published `trellis-rs` facade, and the
//! administration API is projected as generated source under `runtime_api`.
//!
//! Applications own the generated clients and services they connect; stop those
//! transports before calling [`TrellisTestRuntime::shutdown`].
//!
//! ```no_run
//! # async fn example() -> Result<(), trellis_test::TrellisTestError> {
//! use trellis_test::TrellisTestRuntime;
//! let mut runtime = TrellisTestRuntime::builder().start().await?;
//! let _url = runtime.trellis_url();
//! runtime.shutdown().await?;
//! # Ok(())
//! # }
//! ```

// The public API returns the structured `TrellisTestError` by value, so the
// large-error lint does not apply to this crate's stable signature.
#![allow(clippy::result_large_err)]

#[path = "runtime_api/lib.rs"]
#[allow(warnings, clippy::all, clippy::pedantic)]
mod runtime_api;

// The generated administration source uses crate-root paths such as
// `crate::apis`, `crate::types`, `crate::__types`, and `crate::PaginationError`.
// Re-export them at the crate root without exposing them to users.
#[allow(unused_imports)]
pub(crate) use runtime_api::*;

mod admin;
mod error;
mod identity;
mod process;
mod runtime;
mod sandbox;

pub use error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
pub use identity::{InstalledParticipant, TestClientIdentity, TestServiceIdentity};
pub use runtime::{NatsSource, TestTimeouts, TrellisTestRuntime, TrellisTestRuntimeBuilder};
pub use sandbox::WorkdirRetention;
