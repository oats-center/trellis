//! Wall-clock time for persisted timestamps and protocol expiries.

/// Current Unix time in whole seconds.
///
/// `OffsetDateTime::now_utc` cannot fail, unlike `SystemTime::duration_since`,
/// so callers need no error path for an unrepresentable clock.
pub(crate) fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
