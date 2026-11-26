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

/// A stream info reply's oldest record is dated by the broker's clock. Only
/// the broker's own clock in the same reply can say how old it is: a reply
/// without one, or with one that doesn't parse, leaves the age unknown,
/// however old the record looks by this host's clock, so clock skew never
/// reads as a stall.
#[test]
fn a_stall_is_judged_only_by_the_brokers_own_clock() {
    use super::{ExpiryCheck, StreamInfoReply};
    use std::time::Duration;
    let ttl = Duration::from_secs(15);
    let renew = Duration::from_secs(5);
    let judge = |reply: serde_json::Value| {
        serde_json::from_value::<StreamInfoReply>(reply)
            .unwrap()
            .expiry("KV_trellis_runtime_leases", ttl, renew)
    };
    // A record written long before this host's clock.
    let state = serde_json::json!({ "messages": 1, "first_ts": "2001-01-01T00:00:00Z" });
    for reply in [
        serde_json::json!({ "state": state }),
        serde_json::json!({ "ts": null, "state": state }),
        serde_json::json!({ "ts": "yesterday", "state": state }),
    ] {
        assert_eq!(judge(reply.clone()), ExpiryCheck::Unknown, "{reply}");
    }
    // By the broker's clock, the same record is healthy or stalled.
    assert_eq!(
        judge(serde_json::json!({ "ts": "2001-01-01T00:00:10Z", "state": state })),
        ExpiryCheck::Expiring
    );
    let ExpiryCheck::Stalled(stalled) =
        judge(serde_json::json!({ "ts": "2001-01-01T00:00:21Z", "state": state }))
    else {
        panic!("a record older than its lifetime by the broker's clock is a stall");
    };
    assert_eq!(stalled.stream, "KV_trellis_runtime_leases");
}
