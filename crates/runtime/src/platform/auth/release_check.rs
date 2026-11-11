//! The running server's version, and for administrators the newest Trellis
//! release on its channel. GitHub is asked only when an administrator asks,
//! at most once an hour, so a server nobody looks at never calls out.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use trellis_runtime_apis::apis::trellis_auth_v1::Provider;
use trellis_runtime_apis::types::{AuthRuntimeUpdate, AuthRuntimeVersionResponse};
use trellis_self_update::{check_for_update, ReleaseChannel, SelfUpdateTarget};

use super::rpc::{caller, rpc_error};
use super::Administration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TARGET: SelfUpdateTarget =
    SelfUpdateTarget::new("oats-center", "trellis", "trellis-server", VERSION);
/// How long an answer, or a failed check, is reused before GitHub is asked again.
const REUSE: Duration = Duration::from_secs(3600);
/// How long an administrator's request waits for GitHub.
const WAIT: Duration = Duration::from_secs(5);

/// The last answer: when it was found, and the newer release with the Unix
/// second it was checked, if there was one.
type Answer = (Instant, Option<(String, i64)>);

/// Serve `Runtime.Version` on the platform provider.
pub(super) fn register_rpc<P: trellis_rs::generated::ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
) {
    let last: Arc<Mutex<Option<Answer>>> = Arc::default();
    provider.register_runtime_version(move |context, _input| {
        let last = last.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            let administrator = Administration::ALL
                .iter()
                .any(|administration| caller(&context, Some(*administration)).is_ok());
            let update = if administrator {
                newer_release(&last).await
            } else {
                None
            };
            Ok(AuthRuntimeVersionResponse {
                version: VERSION.to_owned(),
                update: update
                    .map(|(version, checked)| {
                        Ok::<_, super::AuthError>(AuthRuntimeUpdate {
                            version,
                            checked_at: super::sqlite::timestamp(checked)?,
                            extra: Default::default(),
                        })
                    })
                    .transpose()
                    .map_err(rpc_error)?,
                extra: Default::default(),
            })
        }
    });
}

/// The newer release, reusing an answer younger than an hour. Concurrent
/// callers wait for the one check in flight rather than each asking GitHub.
async fn newer_release(last: &Mutex<Option<Answer>>) -> Option<(String, i64)> {
    let mut last = last.lock().await;
    if let Some((found, newer)) = last.as_ref() {
        if found.elapsed() < REUSE {
            return newer.clone();
        }
    }
    let check = tokio::task::spawn_blocking(|| {
        check_for_update(TARGET, ReleaseChannel::for_version(VERSION))
    });
    let newer = match tokio::time::timeout(WAIT, check).await {
        Ok(Ok(Ok(check))) => check
            .needs_update
            .then(|| (check.latest_version, crate::clock::unix_now())),
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "could not check GitHub for a newer Trellis release");
            None
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "the Trellis release check failed");
            None
        }
        Err(_) => {
            tracing::warn!("GitHub did not answer the Trellis release check within 5s");
            None
        }
    };
    *last = Some((Instant::now(), newer.clone()));
    newer
}
