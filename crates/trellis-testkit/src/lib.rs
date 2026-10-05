//! Isolated live Trellis runtimes for Rust integration tests.
//!
//! [`TrellisTestRuntime`] links and runs the real production Trellis runtime
//! in-process, with no `trellis` CLI or `trellis-server` prerequisite. It generates a
//! real bootstrap bundle, starts managed NATS, reserves four loopback ports
//! automatically, seeds a sandbox administrator and completes normal public login,
//! and exposes helpers that install participants and provision identities using
//! the generated administration API.
//!
//! Administration uses the same generated production APIs as the runtime.
//!
//! Applications own the generated clients and services they connect; stop those
//! transports before calling [`TrellisTestRuntime::shutdown`].
//!
//! ```no_run
//! # async fn example() -> Result<(), trellis_testkit::TrellisTestError> {
//! use trellis_testkit::TrellisTestRuntime;
//! let mut runtime = TrellisTestRuntime::builder().start().await?;
//! let _url = runtime.trellis_url();
//! runtime.shutdown().await?;
//! # Ok(())
//! # }
//! ```

// The public API returns the structured `TrellisTestError` by value, so the
// large-error lint does not apply to this crate's stable signature.
#![allow(clippy::result_large_err)]

mod admin;
mod error;
mod identity;
mod runtime;
mod sandbox;

pub use error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
pub use identity::{InstalledParticipant, TestClientIdentity, TestServiceIdentity};
pub use runtime::{NatsSource, TestTimeouts, TrellisTestRuntime, TrellisTestRuntimeBuilder};
pub use sandbox::{remove_retained_workdirs, WorkdirRetention};
