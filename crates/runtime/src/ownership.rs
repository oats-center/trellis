//! Runtime-wide singleton ownership lifecycle.
//!
//! Each selected owner group acquires a NATS KV lease before its subsystem starts.
//! After initial verification, one task owns each lease for its complete lifetime:
//! renewal, failure reporting, and release. A renewal that fails without losing
//! the lease, such as a KV update timing out on a loaded host, is tried again.
//! Retries are bounded by the last renewal the broker confirmed, not by when the
//! task woke: ownership is given up one renewal interval before that renewal's
//! lease would expire, so owned work stops before another runtime can take it.
//!
//! The supervisor runs each selected group on its own. A group that cannot own
//! its lease stands by: it tries again each renewal interval until the lease is
//! free, whether another runtime holds it or the broker cannot answer. A group
//! that loses its lease stops its work and returns here, while the other groups
//! carry on, so the process never exits over ownership.

use std::collections::{hash_map::DefaultHasher, BTreeMap};
use std::hash::{Hash, Hasher};
use std::time::Duration;

use async_nats::jetstream;
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::leases::{LeaseError, LeaseFence, LeaseGuard, LeaseKey, LeaseManager};
use crate::shutdown::StopHandle;
use crate::supervisor::{RuntimeError, OWNERSHIP_SHUTDOWN_TIMEOUT};
use crate::{ResolvedLeasesConfig, RuntimeMode, SubsystemName};

/// Pause between renewal attempts within one round after a backend failure.
const RENEWAL_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Stable runtime singleton owner group.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum OwnerGroup {
    Platform,
    Jobs,
    Health,
    Events,
}

impl OwnerGroup {
    pub(crate) fn for_mode(mode: RuntimeMode) -> &'static [Self] {
        match mode {
            RuntimeMode::All => &[Self::Platform, Self::Jobs, Self::Health, Self::Events],
            RuntimeMode::Platform => &[Self::Platform],
            RuntimeMode::Jobs => &[Self::Jobs],
            RuntimeMode::Health => &[Self::Health],
            RuntimeMode::Events => &[Self::Events],
        }
    }

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Platform => "platform.owner",
            Self::Jobs => "jobs.owner",
            Self::Health => "health.owner",
            Self::Events => "events.owner",
        }
    }

    pub(crate) const fn subsystem(self) -> SubsystemName {
        match self {
            Self::Platform => SubsystemName::Platform,
            Self::Jobs => SubsystemName::Jobs,
            Self::Health => SubsystemName::Health,
            Self::Events => SubsystemName::Events,
        }
    }
}

/// Fixed ownership information passed to a selected subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnerContext {
    pub(crate) group: OwnerGroup,
    pub(crate) key: LeaseKey,
    pub(crate) fence: LeaseFence,
}

#[derive(Debug)]
struct HeldLease {
    group: OwnerGroup,
    guard: LeaseGuard,
}

/// Owns all selected runtime leases and their renewal tasks.
#[derive(Debug)]
pub(crate) struct RuntimeOwnership {
    owner_id: String,
    owners: BTreeMap<OwnerGroup, OwnerContext>,
    stop: StopHandle,
    leases: JoinSet<Result<(), RuntimeError>>,
}

impl RuntimeOwnership {
    /// Opens the runtime lease bucket for this process's owner identity.
    pub(crate) async fn open(
        client: async_nats::Client,
        config: &ResolvedLeasesConfig,
        owner_id: String,
    ) -> Result<LeaseManager, RuntimeError> {
        LeaseManager::open(jetstream::new(client), config, owner_id.clone())
            .await
            .map_err(|source| RuntimeError::LeaseBucketOpen {
                owner_id,
                source: Box::new(source),
            })
    }

    /// Acquires every lease in `groups`, standing by until each is free.
    ///
    /// Another runtime holding a lease and a broker that cannot answer are both
    /// waited out, one attempt per renewal interval; `standing_by` runs after
    /// each failed attempt. Returns `None` once `stop` fires.
    pub(crate) async fn acquire_when_free(
        manager: &LeaseManager,
        groups: &[OwnerGroup],
        stop: &StopHandle,
        standing_by: impl Fn(),
    ) -> Option<Self> {
        let mut last_failure = None;
        loop {
            if stop.is_stopped() {
                return None;
            }
            // An attempt runs to completion so a stop never strands a lease it
            // took; each broker request inside it is bounded.
            match Self::acquire(manager, groups).await {
                Ok(ownership) => {
                    if last_failure.is_some() {
                        tracing::info!("runtime owns its work again; starting it");
                    }
                    return Some(ownership);
                }
                Err(error) => {
                    let failure = error.to_string();
                    if last_failure.as_ref() != Some(&failure) {
                        tracing::warn!(
                            error = %error,
                            "runtime is standing by until it can own its work"
                        );
                    }
                    last_failure = Some(failure);
                    standing_by();
                }
            }
            let retry =
                manager.renew + renewal_jitter(&manager.owner_id, manager.renew, manager.ttl);
            tokio::select! {
                () = stop.stopped() => return None,
                () = tokio::time::sleep(retry) => {}
            }
        }
    }

    async fn acquire(manager: &LeaseManager, groups: &[OwnerGroup]) -> Result<Self, RuntimeError> {
        let owner_id = manager.owner_id.clone();
        let mut held = Vec::new();
        let mut owners = BTreeMap::new();

        for &group in groups {
            let key = LeaseKey::new(group.key());
            match manager.acquire(key.clone()).await {
                Ok(guard) => {
                    record_runtime_lease_event(group, "acquire", "ok");
                    owners.insert(
                        group,
                        OwnerContext {
                            group,
                            key,
                            fence: guard.fence(),
                        },
                    );
                    held.push(HeldLease { group, guard });
                }
                Err(source) => {
                    record_runtime_lease_event(group, "acquire", "error");
                    let primary = map_acquisition_error(group, key, &owner_id, source);
                    if let Err(cleanup) = release_held(manager, &owner_id, &mut held).await {
                        tracing::error!(error = %cleanup, "failed to clean up partially acquired runtime ownership");
                    }
                    return Err(primary);
                }
            }
        }

        let verification_started = Instant::now();
        if let Err(primary) = verify_held(manager, &owner_id, &mut held).await {
            if let Err(cleanup) = release_held(manager, &owner_id, &mut held).await {
                tracing::error!(error = %cleanup, "failed to clean up ownership after acquisition verification");
            }
            return Err(primary);
        }

        let stop = StopHandle::new();
        let mut leases = JoinSet::new();
        for lease in held {
            leases.spawn(run_owned_lease(
                manager.clone(),
                owner_id.clone(),
                lease,
                stop.clone(),
                verification_started,
            ));
        }

        Ok(Self {
            owner_id,
            owners,
            stop,
            leases,
        })
    }

    pub(crate) fn contexts(&self) -> BTreeMap<OwnerGroup, OwnerContext> {
        self.owners.clone()
    }

    pub(crate) async fn wait_for_renewal_failure(&mut self) -> RuntimeError {
        match self.leases.join_next().await {
            Some(Ok(Err(error))) => error,
            Some(Ok(Ok(()))) | None => RuntimeError::OwnerRenewalTaskExited {
                owner_id: self.owner_id.clone(),
            },
            Some(Err(source)) => RuntimeError::OwnerRenewalTaskFailed {
                owner_id: self.owner_id.clone(),
                source,
            },
        }
    }

    /// A renewal failure that has already ended a lease's task, without waiting.
    pub(crate) fn renewal_failure_now(&mut self) -> Option<RuntimeError> {
        Some(match self.leases.try_join_next()? {
            Ok(Err(error)) => error,
            Ok(Ok(())) => RuntimeError::OwnerRenewalTaskExited {
                owner_id: self.owner_id.clone(),
            },
            Err(source) => RuntimeError::OwnerRenewalTaskFailed {
                owner_id: self.owner_id.clone(),
                source,
            },
        })
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), RuntimeError> {
        self.stop.stop();
        let deadline = Instant::now() + OWNERSHIP_SHUTDOWN_TIMEOUT;
        let mut first_error = None;

        while !self.leases.is_empty() {
            match tokio::time::timeout_at(deadline, self.leases.join_next()).await {
                Ok(Some(Ok(Ok(())))) => {}
                Ok(Some(Ok(Err(error)))) => record_error(&mut first_error, error),
                Ok(Some(Err(source))) => record_error(
                    &mut first_error,
                    RuntimeError::OwnerRenewalTaskFailed {
                        owner_id: self.owner_id.clone(),
                        source,
                    },
                ),
                Ok(None) => break,
                Err(_) => {
                    self.leases.abort_all();
                    while self.leases.join_next().await.is_some() {}
                    record_error(
                        &mut first_error,
                        RuntimeError::OwnerRenewalShutdownTimeout {
                            owner_id: self.owner_id.clone(),
                        },
                    );
                    break;
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }
}

async fn verify_held(
    manager: &LeaseManager,
    owner_id: &str,
    held: &mut [HeldLease],
) -> Result<(), RuntimeError> {
    let mut renewals = FuturesUnordered::new();
    for lease in held {
        let group = lease.group;
        let key = lease.guard.key().clone();
        let guard = &mut lease.guard;
        renewals.push(async move {
            manager
                .renew(guard)
                .await
                .map_err(|source| map_renewal_error(group, key, owner_id, source))
        });
    }

    let verify = async {
        while let Some(result) = renewals.next().await {
            result?;
        }
        Ok(())
    };
    match tokio::time::timeout(
        renewal_timeout(owner_id, manager.renew, manager.ttl),
        verify,
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(RuntimeError::OwnerRenewalRoundTimeout {
            owner_id: owner_id.to_owned(),
        }),
    }
}

/// Renews one lease until stopped or until ownership can no longer be proven.
///
/// `confirmed` is when the last renewal the broker accepted was sent, so the
/// broker's lease lasts at least `ttl` from it. Every deadline is taken from that
/// instant rather than from when this task runs, because a task that wakes late
/// on a loaded host has not extended the lease.
async fn run_owned_lease(
    manager: LeaseManager,
    owner_id: String,
    mut lease: HeldLease,
    stop: StopHandle,
    mut confirmed: Instant,
) -> Result<(), RuntimeError> {
    let jitter = renewal_jitter(&owner_id, manager.renew, manager.ttl);
    let renewal_result = 'rounds: loop {
        let next_renewal = confirmed + manager.renew + jitter;
        tokio::select! {
            () = stop.stopped() => break Ok(()),
            () = tokio::time::sleep_until(next_renewal) => {}
        }

        // A backend failure, such as a KV update that timed out on a loaded
        // host, leaves the lease ours until it expires, so the renewal is tried
        // again. Ownership is given up on proof that the lease is gone or
        // another owner's, or one renewal interval before the confirmed lease
        // would expire, which leaves the supervisor that long to stop the work.
        let give_up = confirmed + manager.ttl.saturating_sub(manager.renew);
        loop {
            let attempt = Instant::now();
            let failure =
                match tokio::time::timeout_at(give_up, manager.renew(&mut lease.guard)).await {
                    Ok(Ok(())) => {
                        confirmed = attempt;
                        break;
                    }
                    Ok(Err(source @ LeaseError::Backend { .. })) => source,
                    Ok(Err(source)) => {
                        break 'rounds Err(map_renewal_error(
                            lease.group,
                            lease.guard.key().clone(),
                            &owner_id,
                            source,
                        ));
                    }
                    Err(_) => {
                        break 'rounds Err(RuntimeError::OwnerRenewalRoundTimeout {
                            owner_id: owner_id.clone(),
                        });
                    }
                };
            let retry = Instant::now() + RENEWAL_RETRY_DELAY;
            if retry >= give_up {
                break 'rounds Err(map_renewal_error(
                    lease.group,
                    lease.guard.key().clone(),
                    &owner_id,
                    failure,
                ));
            }
            tracing::warn!(
                subsystem = %lease.group.subsystem(),
                error = %failure,
                "runtime owner lease renewal failed; retrying before the lease expires"
            );
            tokio::select! {
                () = stop.stopped() => break 'rounds Ok(()),
                () = tokio::time::sleep_until(retry) => {}
            }
        }
    };

    match renewal_result {
        // Report the loss at once. The release is fenced by the lease revision,
        // so finishing it in the background cannot remove a later owner's lease.
        Err(primary) => {
            tokio::spawn(async move {
                if let Err(release) = release_one(&manager, &owner_id, lease).await {
                    tracing::error!(error = %release, "runtime ownership release also failed");
                }
            });
            Err(primary)
        }
        Ok(()) => release_one(&manager, &owner_id, lease).await,
    }
}

async fn release_one(
    manager: &LeaseManager,
    owner_id: &str,
    lease: HeldLease,
) -> Result<(), RuntimeError> {
    let group = lease.group;
    let key = lease.guard.key().clone();
    let release = tokio::time::timeout(OWNERSHIP_SHUTDOWN_TIMEOUT, manager.release(lease.guard))
        .await
        .unwrap_or_else(|_| {
            Err(LeaseError::Backend {
                key: Some(key.clone()),
                operation: "release",
                message: "operation exceeded shutdown bound".to_owned(),
            })
        });
    record_runtime_lease_event(
        group,
        "release",
        if release.is_ok() { "ok" } else { "error" },
    );
    release.map_err(|source| RuntimeError::OwnerRelease {
        subsystem: group.subsystem(),
        key,
        owner_id: owner_id.to_owned(),
        source: Box::new(source),
    })
}

/// Records one completed singleton lease action with its bounded outcome.
fn record_runtime_lease_event(group: OwnerGroup, action: &'static str, outcome: &'static str) {
    let component = match group {
        OwnerGroup::Platform => "platform",
        OwnerGroup::Jobs => "jobs",
        OwnerGroup::Health => "health",
        OwnerGroup::Events => "events",
    };
    trellis_rs::telemetry::instruments::add_counter(
        trellis_rs::telemetry::instruments::CounterFamily::RuntimeLeaseEvents,
        1,
        &[
            trellis_rs::telemetry::KeyValue::new("trellis.component", component),
            trellis_rs::telemetry::KeyValue::new("trellis.action", action),
            trellis_rs::telemetry::KeyValue::new("trellis.outcome", outcome),
        ],
    );
}

async fn release_held(
    manager: &LeaseManager,
    owner_id: &str,
    held: &mut Vec<HeldLease>,
) -> Result<(), RuntimeError> {
    let mut first_error = None;
    while let Some(lease) = held.pop() {
        if let Err(error) = release_one(manager, owner_id, lease).await {
            record_error(&mut first_error, error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn record_error(first_error: &mut Option<RuntimeError>, error: RuntimeError) {
    if first_error.is_some() {
        tracing::error!(error = %error, "additional runtime ownership failure");
    } else {
        *first_error = Some(error);
    }
}

fn map_acquisition_error(
    group: OwnerGroup,
    key: LeaseKey,
    owner_id: &str,
    source: LeaseError,
) -> RuntimeError {
    if matches!(source, LeaseError::Held { .. }) {
        RuntimeError::OwnerHeld {
            subsystem: group.subsystem(),
            key,
            owner_id: owner_id.to_owned(),
            source: Box::new(source),
        }
    } else {
        RuntimeError::OwnerAcquire {
            subsystem: group.subsystem(),
            key,
            owner_id: owner_id.to_owned(),
            source: Box::new(source),
        }
    }
}

fn map_renewal_error(
    group: OwnerGroup,
    key: LeaseKey,
    owner_id: &str,
    source: LeaseError,
) -> RuntimeError {
    RuntimeError::OwnerRenewal {
        subsystem: group.subsystem(),
        key,
        owner_id: owner_id.to_owned(),
        source: Box::new(source),
    }
}

fn renewal_timeout(owner_id: &str, renew: Duration, ttl: Duration) -> Duration {
    ttl.saturating_sub(renew + renewal_jitter(owner_id, renew, ttl))
}

fn renewal_jitter(owner_id: &str, renew: Duration, ttl: Duration) -> Duration {
    let margin = ttl
        .saturating_sub(renew)
        .saturating_sub(Duration::from_millis(1));
    let bound = (renew / 5).min(margin).as_millis() as u64;
    if bound == 0 {
        return Duration::ZERO;
    }
    let mut hasher = DefaultHasher::new();
    owner_id.hash(&mut hasher);
    Duration::from_millis(hasher.finish() % (bound + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Starts a real JetStream broker and a lease manager whose KV requests
    /// give up quickly, as they do on a loaded host.
    async fn broker_and_manager(
        directory: &std::path::Path,
    ) -> (tokio::process::Child, LeaseManager, ResolvedLeasesConfig) {
        use tokio::io::AsyncBufReadExt as _;
        let reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
        let port = reservation.port();
        let mut server = tokio::process::Command::new(
            std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into()),
        )
        .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
        .arg(directory)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
        let mut logs = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(line) = logs.next_line().await.unwrap() {
                if line.contains("Server is ready") {
                    return;
                }
            }
            panic!("NATS exited before readiness");
        })
        .await
        .unwrap();
        drop(reservation);
        let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
            .await
            .unwrap();
        let mut jetstream = jetstream::new(nats);
        jetstream.set_timeout(Duration::from_millis(300));
        let config = ResolvedLeasesConfig {
            bucket: "trellis_leases".into(),
            replicas: 1,
            ttl_ms: 6_000,
            renew_ms: 1_000,
        };
        let manager = LeaseManager::open(jetstream, &config, "owner-a")
            .await
            .unwrap();
        (server, manager, config)
    }

    fn signal_broker(server: &tokio::process::Child, signal: &str) {
        let pid = server.id().unwrap().to_string();
        assert!(std::process::Command::new("kill")
            .args([signal, &pid])
            .status()
            .unwrap()
            .success());
    }

    /// A broker that stalls longer than one KV request timeout, but less than
    /// the lease's lifetime, must not cost the runtime its ownership.
    #[tokio::test]
    async fn a_renewal_that_times_out_is_retried_before_the_lease_expires() {
        let directory = tempfile::tempdir().unwrap();
        let (server, manager, _) = broker_and_manager(directory.path()).await;
        let confirmed = Instant::now();
        let guard = manager
            .acquire(LeaseKey::new("health.owner"))
            .await
            .unwrap();
        let stop = StopHandle::new();
        let mut task = tokio::spawn(run_owned_lease(
            manager.clone(),
            "owner-a".into(),
            HeldLease {
                group: OwnerGroup::Health,
                guard,
            },
            stop.clone(),
            confirmed,
        ));

        // Freeze the broker across the first renewal.
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal_broker(&server, "-STOP");
        tokio::time::sleep(Duration::from_secs(2)).await;
        signal_broker(&server, "-CONT");

        // Ownership survives the stall and keeps renewing afterwards.
        let outcome = tokio::time::timeout(Duration::from_secs(4), &mut task).await;
        assert!(
            outcome.is_err(),
            "ownership ended during a short broker stall: {outcome:?}"
        );
        stop.stop();
        task.await.unwrap().unwrap();
    }

    /// A renewal task that wakes late has not extended the lease. If the broker
    /// then cannot answer, ownership must still end before the lease the broker
    /// last confirmed expires, or another runtime could take the work while
    /// this one still runs it.
    #[tokio::test]
    async fn a_late_renewal_wake_still_gives_ownership_up_before_the_lease_expires() {
        let directory = tempfile::tempdir().unwrap();
        let (server, manager, config) = broker_and_manager(directory.path()).await;
        let ttl = Duration::from_millis(config.ttl_ms);
        let confirmed = Instant::now();
        let guard = manager
            .acquire(LeaseKey::new("health.owner"))
            .await
            .unwrap();
        let mut task = tokio::spawn(run_owned_lease(
            manager.clone(),
            "owner-a".into(),
            HeldLease {
                group: OwnerGroup::Health,
                guard,
            },
            StopHandle::new(),
            confirmed,
        ));
        signal_broker(&server, "-STOP");
        // Hold this test's only executor thread past the first renewal, so the
        // renewal task wakes about two seconds late.
        std::thread::sleep(Duration::from_secs(3));

        let outcome = tokio::time::timeout(ttl, &mut task).await;
        let given_up = confirmed.elapsed();
        signal_broker(&server, "-CONT");
        let result = outcome
            .expect("ownership outlived the confirmed lease")
            .unwrap();
        assert!(
            result.is_err(),
            "renewal succeeded against a stopped broker"
        );
        assert!(
            given_up < ttl,
            "ownership ended {given_up:?} after the last confirmed renewal, past the {ttl:?} lease"
        );
    }

    #[test]
    fn mode_owner_groups_use_stable_subsystem_order_and_keys() {
        assert_eq!(
            OwnerGroup::for_mode(RuntimeMode::All),
            &[
                OwnerGroup::Platform,
                OwnerGroup::Jobs,
                OwnerGroup::Health,
                OwnerGroup::Events,
            ]
        );
        assert_eq!(OwnerGroup::Platform.key(), "platform.owner");
        assert_eq!(OwnerGroup::Jobs.key(), "jobs.owner");
        assert_eq!(OwnerGroup::Health.key(), "health.owner");
        assert_eq!(OwnerGroup::Events.key(), "events.owner");
    }

    #[test]
    fn owner_held_and_lease_loss_map_to_typed_errors() {
        let key = LeaseKey::new("jobs.owner");
        let held = map_acquisition_error(
            OwnerGroup::Jobs,
            key.clone(),
            "owner-1",
            LeaseError::Held { key: key.clone() },
        );
        assert!(matches!(held, RuntimeError::OwnerHeld { .. }));

        let lost = map_renewal_error(
            OwnerGroup::Jobs,
            key.clone(),
            "owner-1",
            LeaseError::Stale { key },
        );
        assert!(matches!(lost, RuntimeError::OwnerRenewal { .. }));
    }

    #[test]
    fn renewal_schedule_stays_inside_lease_ttl() {
        let renew = Duration::from_secs(5);
        let ttl = Duration::from_secs(15);
        let jitter = renewal_jitter("owner-1", renew, ttl);
        assert!(renew + jitter < ttl);
        assert_eq!(renew + jitter + renewal_timeout("owner-1", renew, ttl), ttl);
    }
}
