//! Snapshot projection at the actual SQLite effect lifecycle boundary.

use rusqlite::{Connection, OpenFlags};

use super::{fixture, SqliteAuthorizationStore, NOW};

#[test]
fn read_only_snapshots_track_issued_authority_effects_and_completion() {
    let f = fixture();
    f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let reader = SqliteAuthorizationStore::from_connection(
        Connection::open_with_flags(
            f.directory.path().join("platform.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
        f.store.signer.clone(),
        f.store.settings.clone(),
    )
    .unwrap();
    let snapshot = reader.telemetry_snapshot(NOW + 10).unwrap();
    assert!(snapshot.post_commit_pending > 0);
    assert_eq!(snapshot.post_commit_oldest_age_seconds, 10.0);
    let effects = f.store.claim_effects(256, NOW + 10, 30).unwrap();
    let effect = effects
        .iter()
        .find(|effect| effect.kind == "authority_publish")
        .unwrap();
    // Claiming for delivery is not completion and must not erase backlog.
    assert_eq!(
        reader
            .telemetry_snapshot(NOW + 10)
            .unwrap()
            .post_commit_pending,
        snapshot.post_commit_pending
    );
    f.store.complete_effect(effect, NOW + 10).unwrap();
    assert_eq!(
        reader
            .telemetry_snapshot(NOW + 10)
            .unwrap()
            .post_commit_pending,
        snapshot.post_commit_pending - 1
    );
    // Drain through ordinary claims and acknowledgements, including successors
    // that only become claimable after their predecessor completes.
    for effect in effects
        .iter()
        .filter(|pending| pending.action_id != effect.action_id)
    {
        f.store.complete_effect(effect, NOW + 10).unwrap();
    }
    loop {
        let effects = f.store.claim_effects(256, NOW + 10, 30).unwrap();
        if effects.is_empty() {
            break;
        }
        for effect in effects {
            f.store.complete_effect(&effect, NOW + 10).unwrap();
        }
    }
    let drained = reader.telemetry_snapshot(NOW + 20).unwrap();
    assert_eq!(drained.post_commit_pending, 0);
    assert_eq!(drained.post_commit_oldest_age_seconds, 0.0);
}
