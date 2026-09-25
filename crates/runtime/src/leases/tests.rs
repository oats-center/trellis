use crate::{ConfigError, LeasesConfig};

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
