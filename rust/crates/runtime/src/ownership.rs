//! Runtime-wide singleton ownership lifecycle.
//!
//! Each selected owner group acquires a NATS KV lease before its subsystem starts.
//! After initial verification, one task owns each lease for its complete lifetime:
//! renewal, failure reporting, and release. There is no standby or reacquisition.

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
    pub(crate) async fn acquire(
        client: async_nats::Client,
        config: &ResolvedLeasesConfig,
        owner_id: String,
        mode: RuntimeMode,
    ) -> Result<Self, RuntimeError> {
        let manager = LeaseManager::open(jetstream::new(client), config, owner_id.clone())
            .await
            .map_err(|source| RuntimeError::LeaseBucketOpen {
                owner_id: owner_id.clone(),
                source: Box::new(source),
            })?;
        let mut held = Vec::new();
        let mut owners = BTreeMap::new();

        for &group in OwnerGroup::for_mode(mode) {
            let key = LeaseKey::new(group.key());
            match manager.acquire(key.clone()).await {
                Ok(guard) => {
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
                    let primary = map_acquisition_error(group, key, &owner_id, source);
                    if let Err(cleanup) = release_held(&manager, &owner_id, &mut held).await {
                        tracing::error!(error = %cleanup, "failed to clean up partially acquired runtime ownership");
                    }
                    return Err(primary);
                }
            }
        }

        let verification_started = Instant::now();
        if let Err(primary) = verify_held(&manager, &owner_id, &mut held).await {
            if let Err(cleanup) = release_held(&manager, &owner_id, &mut held).await {
                tracing::error!(error = %cleanup, "failed to clean up ownership after acquisition verification");
            }
            return Err(primary);
        }

        let stop = StopHandle::new();
        let first_renewal =
            first_renewal_deadline(verification_started, &owner_id, manager.renew, manager.ttl);
        let mut leases = JoinSet::new();
        for lease in held {
            leases.spawn(run_owned_lease(
                manager.clone(),
                owner_id.clone(),
                lease,
                stop.clone(),
                first_renewal,
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

async fn run_owned_lease(
    manager: LeaseManager,
    owner_id: String,
    mut lease: HeldLease,
    stop: StopHandle,
    mut next_renewal: Instant,
) -> Result<(), RuntimeError> {
    let renewal_result = loop {
        tokio::select! {
            () = stop.stopped() => break Ok(()),
            () = tokio::time::sleep_until(next_renewal) => {}
        }

        match tokio::time::timeout(
            renewal_timeout(&owner_id, manager.renew, manager.ttl),
            manager.renew(&mut lease.guard),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(source)) => {
                break Err(map_renewal_error(
                    lease.group,
                    lease.guard.key().clone(),
                    &owner_id,
                    source,
                ));
            }
            Err(_) => {
                break Err(RuntimeError::OwnerRenewalRoundTimeout {
                    owner_id: owner_id.clone(),
                });
            }
        }

        next_renewal = next_renewal
            .checked_add(manager.renew)
            .unwrap_or(next_renewal);
    };

    let release_result = release_one(&manager, &owner_id, lease).await;
    match renewal_result {
        Err(primary) => {
            if let Err(release) = release_result {
                tracing::error!(error = %release, "runtime ownership release also failed");
            }
            Err(primary)
        }
        Ok(()) => release_result,
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
    release.map_err(|source| RuntimeError::OwnerRelease {
        subsystem: group.subsystem(),
        key,
        owner_id: owner_id.to_owned(),
        source: Box::new(source),
    })
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

fn first_renewal_deadline(
    verification_started: Instant,
    owner_id: &str,
    renew: Duration,
    ttl: Duration,
) -> Instant {
    let delay = renew.saturating_add(renewal_jitter(owner_id, renew, ttl));
    verification_started
        .checked_add(delay)
        .unwrap_or(verification_started)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn first_renewal_is_anchored_to_verification_start() {
        let renew = Duration::from_secs(5);
        let ttl = renew * 3;
        let started = Instant::now();
        assert_eq!(
            first_renewal_deadline(started, "owner-1", renew, ttl),
            started + renew + renewal_jitter("owner-1", renew, ttl)
        );
    }
}
