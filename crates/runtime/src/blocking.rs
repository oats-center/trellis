//! Synchronous storage work, kept off the async workers.
//!
//! Store calls take a connection mutex and can wait behind password hashing,
//! IDL compilation, or another transaction. Run on an async worker, that wait
//! stalls every task scheduled there, so store work runs on Tokio's blocking
//! pool through [`spawn`]. Stores call [`assert_off_async_workers`] before they
//! lock, which makes a forgotten hop fail loudly in debug builds.
//!
//! A database another process holds locked makes store work wait out its busy
//! timeout, and work queued behind the connection mutex waits in turn. Work
//! whose caller has gone keeps running all the same, so [`store_work_pending`]
//! counts it for the host to report at exit rather than wait for.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Store work [`spawn`]ed and not yet finished or dropped unrun.
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Number of store calls queued or running on the blocking pool.
///
/// A host that has finished its shutdown reports a nonzero count as it exits:
/// that work's callers have stopped, and SQLite keeps none of what it has not
/// committed.
#[must_use]
pub fn store_work_pending() -> usize {
    PENDING.load(Ordering::Relaxed)
}

/// Counts one piece of [`spawn`]ed work until it finishes or is dropped unrun.
struct Pending;

impl Pending {
    fn start() -> Self {
        PENDING.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        PENDING.fetch_sub(1, Ordering::Relaxed);
    }
}

thread_local! {
    /// Set while [`spawn`]ed work runs on this blocking-pool thread.
    static STORE_WORK: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread for the duration of one piece of store work.
struct StoreWork;

impl StoreWork {
    fn enter() -> Self {
        STORE_WORK.set(true);
        Self
    }
}

impl Drop for StoreWork {
    fn drop(&mut self) {
        STORE_WORK.set(false);
    }
}

/// Run synchronous store work on Tokio's blocking pool.
pub(crate) fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    // A failure in the work is the HTTP request's, when one is being answered.
    let request = crate::http_failure::current();
    let pending = Pending::start();
    tokio::task::spawn_blocking(move || {
        let _pending = pending;
        let _work = StoreWork::enter();
        crate::http_failure::within(request, work)
    })
}

/// In debug builds, panic when a store is used on an async worker of a
/// multi-threaded runtime instead of through [`spawn`]. Threads outside any
/// runtime, and the single-threaded runtimes of unit tests, have no workers to
/// protect.
pub(crate) fn assert_off_async_workers() {
    #[cfg(debug_assertions)]
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
            && !STORE_WORK.get()
        {
            // A panic inside an HTTP connection task only closes that
            // connection, so record where the violation happened first.
            tracing::error!(
                backtrace = %std::backtrace::Backtrace::force_capture(),
                "store used on an async worker"
            );
            panic!("store used on an async worker; run it through `crate::blocking::spawn`");
        }
    }
}
