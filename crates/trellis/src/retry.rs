//! Retrying failed work with bounded backoff, and logging a failure that
//! repeats without flooding the log.
//!
//! Background loops in the runtime and the SDK share these so that every loop
//! waits, classifies and logs the same way (see
//! `design/core/engineering-patterns.md`).

use std::future::Future;
use std::time::{Duration, Instant};

/// What a failure of retried work calls for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    /// It can pass, such as a lock another connection holds, a busy database,
    /// a broker that is not answering yet, or a full disk. The same work is
    /// tried again after a wait.
    Transient,
    /// Trying the same work again fails the same way, such as a constraint
    /// violation or a value of the wrong type. The work is set aside so the
    /// work after it can go on.
    Permanent,
    /// The state the work needs cannot be used, such as a corrupt database
    /// file. The subsystem that owns it stops.
    Unusable,
}

/// The waits between attempts of work that keeps failing: `first` after the
/// first failure, doubling with each further failure up to `longest`, and
/// back to `first` once the work succeeds.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    first: Duration,
    longest: Duration,
    failures: u32,
}

impl Default for Backoff {
    /// 250 milliseconds after the first failure, doubling up to 30 seconds:
    /// the waits of the runtime's background storage and broker work.
    fn default() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_secs(30))
    }
}

impl Backoff {
    /// Waits that start at `first` and double up to `longest`.
    #[must_use]
    pub const fn new(first: Duration, longest: Duration) -> Self {
        Self {
            first,
            longest,
            failures: 0,
        }
    }

    /// The wait the next failure returns, without counting one.
    #[must_use]
    pub fn next_delay(&self) -> Duration {
        self.first
            .saturating_mul(1_u32 << self.failures.min(31))
            .min(self.longest)
    }

    /// Counts a failure and returns how long to wait before trying again.
    pub fn failed(&mut self) -> Duration {
        let delay = self.next_delay();
        self.failures = self.failures.saturating_add(1);
        delay
    }

    /// Starts the waits over after the work succeeded.
    pub fn succeeded(&mut self) {
        self.failures = 0;
    }
}

/// Why [`retry`] gave up on its work, with the failure that ended it.
#[derive(Debug)]
pub enum Halted<E> {
    /// The work failed in a way trying again cannot change.
    Permanent(E),
    /// The state the work needs cannot be used.
    Unusable(E),
    /// The stop signal arrived while the work was waiting to run again after
    /// this failure.
    Stopped(E),
}

/// Runs `step` until it succeeds, waiting by `backoff` after each failure
/// `classify` calls transient.
///
/// `retrying` sees each transient failure with the wait before the next
/// attempt, to log it. A permanent or unusable failure ends the retries at
/// once, and so does `stopped` completing during a wait.
///
/// # Errors
///
/// Returns the failure that ended the retries, as [`Halted`] says why.
pub async fn retry<T, E, F>(
    mut backoff: Backoff,
    mut step: impl FnMut() -> F,
    classify: impl Fn(&E) -> Failure,
    mut retrying: impl FnMut(&E, Duration),
    stopped: impl Future<Output = ()>,
) -> Result<T, Halted<E>>
where
    F: Future<Output = Result<T, E>>,
{
    let mut stopped = std::pin::pin!(stopped);
    loop {
        let error = match step().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        match classify(&error) {
            Failure::Transient => {}
            Failure::Permanent => return Err(Halted::Permanent(error)),
            Failure::Unusable => return Err(Halted::Unusable(error)),
        }
        let delay = backoff.failed();
        retrying(&error, delay);
        tokio::select! {
            () = &mut stopped => return Err(Halted::Stopped(error)),
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// Lets a log line for a repeating failure through when it first happens and
/// then at most once every `every`, counting the repeats held back between.
///
/// [`LogLimit::recovered`] tells when the failure that was logged has ended,
/// so its recovery is logged once too.
#[derive(Clone, Copy, Debug)]
pub struct LogLimit {
    every: Duration,
    last: Option<Instant>,
    held: u64,
}

impl Default for LogLimit {
    /// At most once a minute.
    fn default() -> Self {
        Self::new(Duration::from_secs(60))
    }
}

impl LogLimit {
    /// Lets a line through at most once every `every`.
    #[must_use]
    pub const fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            held: 0,
        }
    }

    /// Whether to log the failure now: `Some` with how many repeats were held
    /// back since the last line, or `None` to hold this one back.
    pub fn admit(&mut self) -> Option<u64> {
        self.admit_at(Instant::now())
    }

    /// [`LogLimit::admit`] for a failure seen at `now`.
    pub fn admit_at(&mut self, now: Instant) -> Option<u64> {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.every)
        {
            self.held += 1;
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.held))
    }

    /// Records that the work succeeded. Returns true when a failure had been
    /// logged since the last success, so its recovery is worth a line; the
    /// next failure is then logged at once.
    pub fn recovered(&mut self) -> bool {
        self.held = 0;
        self.last.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_its_bound_and_restarts_after_success() {
        let mut backoff = Backoff::default();
        let waits: Vec<_> = (0..10).map(|_| backoff.failed().as_millis()).collect();
        assert_eq!(
            waits,
            [250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]
        );
        backoff.succeeded();
        assert_eq!(backoff.next_delay(), Duration::from_millis(250));

        // Many failures never overflow: the wait stays at the bound.
        let mut short = Backoff::new(Duration::from_millis(100), Duration::from_secs(5));
        for _ in 0..100 {
            short.failed();
        }
        assert_eq!(short.failed(), Duration::from_secs(5));
    }

    #[test]
    fn log_limit_logs_once_then_at_its_interval_with_the_held_count() {
        let start = Instant::now();
        let mut limit = LogLimit::new(Duration::from_secs(60));
        assert_eq!(limit.admit_at(start), Some(0));
        assert_eq!(limit.admit_at(start + Duration::from_secs(1)), None);
        assert_eq!(limit.admit_at(start + Duration::from_secs(59)), None);
        assert_eq!(limit.admit_at(start + Duration::from_secs(60)), Some(2));
        assert_eq!(limit.admit_at(start + Duration::from_secs(61)), None);

        // A recovery is reported once, and the next failure logs at once.
        assert!(limit.recovered());
        assert!(!limit.recovered());
        assert_eq!(limit.admit_at(start + Duration::from_secs(62)), Some(0));
    }

    #[tokio::test]
    async fn retry_waits_out_transient_failures_and_stops_on_the_others() {
        let mut attempts = 0;
        let mut waits = Vec::new();
        let result = retry(
            Backoff::new(Duration::from_millis(1), Duration::from_millis(2)),
            || {
                attempts += 1;
                std::future::ready(if attempts < 4 {
                    Err(attempts)
                } else {
                    Ok("done")
                })
            },
            |_| Failure::Transient,
            |_, delay| waits.push(delay.as_millis()),
            std::future::pending(),
        )
        .await;
        assert_eq!(result.ok(), Some("done"));
        assert_eq!(waits, [1, 2, 2]);

        let permanent = retry(
            Backoff::default(),
            || std::future::ready(Err::<(), _>("constraint")),
            |_| Failure::Permanent,
            |_, _| panic!("a permanent failure is not retried"),
            std::future::pending(),
        )
        .await;
        assert!(matches!(permanent, Err(Halted::Permanent("constraint"))));

        let unusable = retry(
            Backoff::default(),
            || std::future::ready(Err::<(), _>("corrupt")),
            |_| Failure::Unusable,
            |_, _| panic!("an unusable failure is not retried"),
            std::future::pending(),
        )
        .await;
        assert!(matches!(unusable, Err(Halted::Unusable("corrupt"))));

        // A stop during the wait ends it at once, with the last failure.
        let stopped = tokio::time::timeout(
            Duration::from_secs(10),
            retry(
                Backoff::new(Duration::from_secs(60), Duration::from_secs(60)),
                || std::future::ready(Err::<(), _>("locked")),
                |_| Failure::Transient,
                |_, _| {},
                std::future::ready(()),
            ),
        )
        .await
        .expect("a stop ends the wait");
        assert!(matches!(stopped, Err(Halted::Stopped("locked"))));
    }
}
