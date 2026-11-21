//! Self-enrollment of service instances with an enrollment secret
//! (design/auth/instance-enrollment.md). A process makes its own identity key,
//! registers it once over HTTPS, then connects through ordinary admission.

use std::path::{Path, PathBuf};
use std::time::Duration;

use trellis_protocol::{ServiceIdentitySessionProofInput, SessionProofInput};

use crate::client::{SessionAuth, TrellisClientError};

/// Environment variable the SDK reads an enrollment secret from when the
/// options name none.
pub const ENROLLMENT_SECRET_ENV: &str = "TRELLIS_ENROLLMENT_SECRET";

/// How a service without a provisioned seed gets its identity.
#[derive(Clone)]
pub(crate) struct Enrollment {
    pub(crate) origin: String,
    pub(crate) secret: Option<String>,
    pub(crate) key_file: Option<PathBuf>,
    pub(crate) title: Option<String>,
}

impl std::fmt::Debug for Enrollment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Enrollment")
            .field("origin", &self.origin)
            .field("secret", &self.secret.as_ref().map(|_| "[redacted]"))
            .field("key_file", &self.key_file)
            .field("title", &self.title)
            .finish()
    }
}

/// What Auth reports about an identity key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdentityState {
    Active,
    Disabled,
    Removed,
    Retired,
    Unregistered,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Make a new random identity seed.
pub(crate) fn new_seed() -> Result<String, TrellisClientError> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()))
}

/// The host name an enrolled instance is titled with by default.
pub(crate) fn default_title() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| "service".into())
}

/// Send one request signed by `seed`'s key, retrying only failures that say
/// nothing about the request. The request ID stays the same across retries,
/// so a lost enrollment response is recovered.
async fn signed_post(
    origin: &str,
    roots: &[PathBuf],
    path: &str,
    seed: &str,
    mut fields: serde_json::Value,
) -> Result<serde_json::Value, TrellisClientError> {
    let identity = SessionAuth::from_seed_base64url(seed)?;
    let request_id = ulid::Ulid::generate().to_string();
    let http = crate::client::http_client_builder(roots)?
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
    let mut delay = Duration::from_millis(250);
    for attempt in 1.. {
        fields["requestId"] = request_id.clone().into();
        fields["iat"] = now_ms().into();
        fields["identityKeyId"] = identity.key_id().into();
        fields["identityPublicKey"] = identity.session_key.clone().into();
        let input = ServiceIdentitySessionProofInput {
            origin: origin.to_owned(),
            unsigned_request: fields.clone(),
        };
        let input = if path == "/auth/service/enroll" {
            SessionProofInput::service_enrollment(input)
        } else {
            SessionProofInput::service_identity_status(input)
        }
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let mut body = fields.clone();
        body["proof"] = serde_json::to_value(identity.sign_session_proof(&input)?)?;
        let sent = http
            .post(format!("{}{path}", origin.trim_end_matches('/')))
            .json(&body)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) if attempt >= 8 => {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    error.to_string(),
                ))
            }
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        if response.status().is_success() {
            return Ok(serde_json::from_slice(
                &crate::client::read_bounded_http_body(response, 64 * 1024).await?,
            )?);
        }
        if response.status().is_server_error() && attempt < 8 {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(5));
            continue;
        }
        let error = crate::client::decode_trellis_http_error(response).await;
        return Err(TrellisClientError::BootstrapHttp {
            status: error.status,
            code: error.code,
        });
    }
    unreachable!("the retry loop returns")
}

/// Register `seed`'s key as a new instance of the deployment the secret
/// belongs to. Repeating it for the same key returns the same instance.
pub(crate) async fn enroll(
    origin: &str,
    roots: &[PathBuf],
    secret: &str,
    seed: &str,
    participant_id: &str,
    participant_digest: &str,
    title: &str,
) -> Result<serde_json::Value, TrellisClientError> {
    signed_post(
        origin,
        roots,
        "/auth/service/enroll",
        seed,
        serde_json::json!({
            "enrollmentSecret": secret,
            "participantId": participant_id,
            "participantDigest": participant_digest,
            "title": title,
        }),
    )
    .await
}

/// Ask Auth what became of `seed`'s identity key.
pub(crate) async fn identity_state(
    origin: &str,
    roots: &[PathBuf],
    seed: &str,
) -> Result<IdentityState, TrellisClientError> {
    let body = signed_post(
        origin,
        roots,
        "/auth/service/identity",
        seed,
        serde_json::json!({}),
    )
    .await?;
    match body["state"].as_str() {
        Some("active") => Ok(IdentityState::Active),
        Some("disabled") => Ok(IdentityState::Disabled),
        Some("removed") => Ok(IdentityState::Removed),
        Some("retired") => Ok(IdentityState::Retired),
        Some("unregistered") => Ok(IdentityState::Unregistered),
        _ => Err(TrellisClientError::Bootstrap(
            "invalid service identity status response".into(),
        )),
    }
}

/// Read a key file's seed, or `None` when there is no file yet.
pub(crate) fn read_key_file(path: &Path) -> Result<Option<String>, TrellisClientError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let seed = text.trim().to_owned();
            SessionAuth::from_seed_base64url(&seed)?;
            Ok(Some(seed))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TrellisClientError::Bootstrap(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

/// Replace the key file with `seed`, readable by its owner only. It is written
/// before the key is enrolled, so a crash between the two never loses a key
/// Auth already registered.
pub(crate) fn write_key_file(path: &Path, seed: &str) -> Result<(), TrellisClientError> {
    use std::io::Write as _;
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        writeln!(file, "{seed}")?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    };
    write().map_err(|error| {
        TrellisClientError::Bootstrap(format!("cannot write {}: {error}", path.display()))
    })
}
