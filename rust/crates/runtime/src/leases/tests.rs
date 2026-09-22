use super::{lease_bucket_config, LeaseFence, LeaseGuard, LeaseKey, LeaseManager};
use crate::{ConfigError, LeasesConfig, ResolvedLeasesConfig};

#[test]
fn lease_manager_uses_config_defaults() {
    let config = LeasesConfig {
        bucket: None,
        replicas: Some(1),
        ttl_ms: None,
        renew_ms: None,
    };

    let resolved = config.resolve().expect("resolve leases");
    let manager = LeaseManager::new(&resolved, "runtime-a");

    assert_eq!(manager.bucket, "trellis_runtime_leases");
    assert_eq!(manager.owner_id, "runtime-a");
    assert_eq!(manager.ttl.as_millis(), 15_000);
    assert_eq!(manager.renew.as_millis(), 5_000);
    assert_eq!(manager.replicas, 1);
}

#[test]
fn lease_manager_uses_resolved_config_directly() {
    let config = ResolvedLeasesConfig {
        bucket: "leases".to_owned(),
        replicas: 3,
        ttl_ms: 30_000,
        renew_ms: 10_000,
    };

    let manager = LeaseManager::new(&config, "runtime-a");

    assert_eq!(manager.bucket, "leases");
    assert_eq!(manager.owner_id, "runtime-a");
    assert_eq!(manager.ttl.as_millis(), 30_000);
    assert_eq!(manager.renew.as_millis(), 10_000);
    assert_eq!(manager.replicas, 3);
}

#[test]
fn lease_manager_uses_config_overrides() {
    let config = LeasesConfig {
        bucket: Some("leases".to_owned()),
        replicas: Some(3),
        ttl_ms: Some(30_000),
        renew_ms: Some(10_000),
    };

    let resolved = config.resolve().expect("resolve leases");
    let manager = LeaseManager::new(&resolved, "runtime-a");

    assert_eq!(manager.bucket, "leases");
    assert_eq!(manager.ttl.as_millis(), 30_000);
    assert_eq!(manager.renew.as_millis(), 10_000);
    assert_eq!(manager.replicas, 3);
}

#[test]
fn lease_resolve_requires_configured_replicas() {
    let config = LeasesConfig {
        bucket: None,
        replicas: None,
        ttl_ms: None,
        renew_ms: None,
    };

    assert!(matches!(
        config.resolve(),
        Err(ConfigError::InvalidLeasesConfig {
            section: "leases",
            field: "replicas",
            reason: "must be configured explicitly"
        })
    ));
}

#[test]
fn lease_resolve_defers_replica_validation_to_nats() {
    let config = LeasesConfig {
        bucket: None,
        replicas: Some(0),
        ttl_ms: None,
        renew_ms: None,
    };

    let resolved = config.resolve().expect("resolve leases");
    let manager = LeaseManager::new(&resolved, "runtime-a");

    assert_eq!(manager.replicas, 0);
}

#[test]
fn lease_resolve_requires_three_renewal_intervals_with_checked_arithmetic() {
    for (ttl_ms, renew_ms) in [(0, 1), (1, 0), (2, 1), (u64::MAX, u64::MAX / 3 + 1)] {
        let error = LeasesConfig {
            bucket: None,
            replicas: Some(1),
            ttl_ms: Some(ttl_ms),
            renew_ms: Some(renew_ms),
        }
        .resolve()
        .expect_err("reject unsafe lease timing");
        assert!(matches!(error, ConfigError::InvalidLeasesConfig { .. }));
    }

    let boundary = LeasesConfig {
        bucket: None,
        replicas: Some(1),
        ttl_ms: Some(u64::MAX),
        renew_ms: Some(u64::MAX / 3),
    }
    .resolve()
    .expect("accept exact safe renewal boundary");
    assert_eq!(boundary.renew_ms * 3, boundary.ttl_ms);
}

#[test]
fn lease_bucket_config_matches_lease_semantics() {
    let config = lease_bucket_config("leases", std::time::Duration::from_secs(15), 3);

    assert_eq!(config.bucket, "leases");
    assert_eq!(config.history, 1);
    assert_eq!(config.max_age.as_secs(), 15);
    assert_eq!(config.num_replicas, 3);
}

#[test]
fn lease_guard_keeps_acquisition_fence_when_revision_advances() {
    let key = LeaseKey::new("jobs.owner");
    let mut guard = LeaseGuard {
        key,
        fence: LeaseFence {
            acquisition_revision: 41,
        },
        current_revision: 41,
    };

    guard.current_revision = 42;

    assert_eq!(guard.fence().acquisition_revision(), 41);
    assert_eq!(guard.current_revision(), 42);
}
