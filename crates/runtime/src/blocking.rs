//! Synchronous storage work, kept off the async workers.
//!
//! Store calls take a connection mutex and can wait behind password hashing,
//! IDL compilation, or another transaction. Run on an async worker, that wait
//! stalls every task scheduled there, so store work runs on Tokio's blocking
//! pool through [`spawn`]. Stores call [`assert_off_async_workers`] before they
//! lock, which makes a forgotten hop fail loudly in debug builds.

use std::cell::Cell;

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
    tokio::task::spawn_blocking(move || {
        let _work = StoreWork::enter();
        work()
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
