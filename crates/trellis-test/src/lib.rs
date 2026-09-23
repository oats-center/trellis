//! Owns a real Trellis runtime for Rust service tests.
//!
//! A [`TrellisTestRuntime`] creates fresh accounts, a real NATS process, real SQLite stores, and a
//! real control plane in one work directory, then stops and removes them. Participants are
//! registered through their generated contracts and callers connect through ordinary generated
//! clients, so tests exercise the same boundaries production does.
//!
//! Tests must use a multi-thread Tokio runtime (`#[tokio::test(flavor = "multi_thread", ...)]`).
//! A current-thread runtime starves the control plane's subsystems: its auth callout handshake
//! fails and startup reports an authorization violation.

mod admin;
mod config;
mod error;
mod ports;
mod runtime;

pub use admin::TrellisTestAdmin;
pub use error::TrellisTestError;
pub use runtime::{
    TrellisTestRuntime, TrellisTestRuntimeOptions, TrellisTestTimeouts, DEFAULT_ADMIN_USERNAME,
};

/// Requests termination of a spawned child when this process exits.
///
/// Linux-only: sets `PR_SET_PDEATHSIG` to `SIGTERM` before `exec`, so a harness child never
/// outlives the test process that spawned it.
pub fn terminate_on_parent_exit(_command: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: `pre_exec` runs between fork and exec, where only async-signal-safe calls are
        // legal. `prctl` and `getppid` are both safe there.
        unsafe {
            _command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "parent already exited before exec",
                    ));
                }
                Ok(())
            });
        }
    }
}
