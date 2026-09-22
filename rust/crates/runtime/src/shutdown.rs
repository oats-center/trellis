use std::future;

use tokio::sync::watch;

/// Cooperative stop handle shared with runtime subsystem tasks.
#[derive(Clone, Debug)]
pub struct StopHandle {
    stopped: watch::Sender<bool>,
}

impl Default for StopHandle {
    fn default() -> Self {
        Self {
            stopped: watch::channel(false).0,
        }
    }
}

impl StopHandle {
    /// Creates a new unset stop handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cooperative shutdown.
    pub fn stop(&self) {
        self.stopped.send_replace(true);
    }

    /// Returns whether cooperative shutdown has been requested.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        *self.stopped.borrow()
    }

    /// Waits until cooperative shutdown is requested.
    pub async fn stopped(&self) {
        let mut stopped = self.stopped.subscribe();
        while !*stopped.borrow_and_update() {
            if stopped.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Waits for the host process shutdown signal.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        /// Waits for SIGTERM on Unix hosts.
        async fn terminate_signal() {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(_) => future::pending::<()>().await,
            }
        }

        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                let _ = result;
            }
            () = terminate_signal() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::StopHandle;

    #[tokio::test]
    async fn stop_notification_handles_stop_before_and_after_wait() {
        let stop = StopHandle::new();
        let waiter = stop.clone();
        let join = tokio::spawn(async move { waiter.stopped().await });
        stop.stop();
        join.await.expect("stop waiter should finish");

        stop.stopped().await;
        assert!(stop.is_stopped());
    }
}
