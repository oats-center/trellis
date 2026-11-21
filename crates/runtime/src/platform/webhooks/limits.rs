//! Webhook request budgets, separate from sign-in's, so a flood of webhooks
//! cannot lock people out.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Most tracked clients or endpoints before the oldest are forgotten.
const MAX_TRACKED: usize = 100_000;

#[derive(Default)]
pub(crate) struct Limits {
    /// Token buckets per deployment and endpoint: tokens and the last refill.
    endpoints: Mutex<HashMap<(String, String), (f64, Instant)>>,
    /// Failed requests per deployment and client address in the current minute.
    failures: Mutex<HashMap<(String, IpAddr), (u64, Instant)>>,
}

impl Limits {
    /// Take one request from the endpoint's budget; `false` when it is spent.
    pub(crate) fn take(&self, key: (String, String), per_second: u64, burst: u64) -> bool {
        let Ok(mut buckets) = self.endpoints.lock() else {
            return true;
        };
        let now = Instant::now();
        if buckets.len() > MAX_TRACKED {
            buckets.clear();
        }
        let (tokens, refilled) = buckets.entry(key).or_insert((burst as f64, now));
        let elapsed = now.duration_since(*refilled).as_secs_f64();
        *tokens = (*tokens + elapsed * per_second as f64).min(burst as f64);
        *refilled = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Whether the client already failed too often this minute.
    pub(crate) fn failing(&self, deployment: &str, client: IpAddr, per_minute: u64) -> bool {
        let Ok(mut failures) = self.failures.lock() else {
            return false;
        };
        let key = (deployment.to_owned(), client);
        match failures.get(&key) {
            Some((count, since)) if since.elapsed() < Duration::from_secs(60) => {
                *count >= per_minute
            }
            Some(_) => {
                failures.remove(&key);
                false
            }
            None => false,
        }
    }

    /// Count one failed request from the client.
    pub(crate) fn failed(&self, deployment: &str, client: IpAddr) {
        let Ok(mut failures) = self.failures.lock() else {
            return;
        };
        if failures.len() > MAX_TRACKED {
            failures.retain(|_, (_, since)| since.elapsed() < Duration::from_secs(60));
        }
        let entry = failures
            .entry((deployment.to_owned(), client))
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() >= Duration::from_secs(60) {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }
}
