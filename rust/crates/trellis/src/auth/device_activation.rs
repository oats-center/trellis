use std::fmt;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::client::{fetch_device_activation, DeviceConnectOptions, TrellisClientError};

type HmacSha256 = Hmac<Sha256>;

const DEVICE_CONFIRMATION_DOMAIN: &str = "trellis-device-confirm/v1";
const CROCKFORD_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

struct DeviceActivationAttempt {
    session_key_seed_base64url: String,
    session_auth: crate::client::SessionAuth,
    connection_id: String,
}

impl DeviceActivationAttempt {
    fn new() -> Result<Self, DeviceActivationError> {
        let (session_key_seed_base64url, _) = crate::auth::generate_session_keypair();
        Self::from_seed(session_key_seed_base64url)
    }

    fn from_seed(session_key_seed_base64url: String) -> Result<Self, DeviceActivationError> {
        let session_auth =
            crate::client::SessionAuth::from_seed_base64url(&session_key_seed_base64url)?;
        Ok(Self {
            session_key_seed_base64url,
            session_auth,
            connection_id: ulid::Ulid::new().to_string(),
        })
    }
}

/// Inputs retained across proof-bound device activation attempts.
pub struct DeviceActivationOptions<'a, C> {
    connect: DeviceConnectOptions<'a, C>,
    activation_key_base64url: &'a str,
    provisioning_secret: Option<&'a str>,
    nonce: String,
}

impl<'a, C> DeviceActivationOptions<'a, C> {
    /// Request the generated participant role; approval assigns deployment and instance.
    pub fn new(connect: DeviceConnectOptions<'a, C>, activation_key_base64url: &'a str) -> Self {
        Self {
            nonce: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
            connect,
            activation_key_base64url,
            provisioning_secret: None,
        }
    }

    /// Supply the one-use provisioning secret for a not-yet-enrolled identity.
    pub const fn with_provisioning_secret(mut self, provisioning_secret: &'a str) -> Self {
        self.provisioning_secret = Some(provisioning_secret);
        self
    }

    /// Replace the generated nonce, primarily for durable local activation state.
    pub fn with_nonce(mut self, nonce: impl Into<String>) -> Self {
        self.nonce = nonce.into();
        self
    }

    /// Attach ready evidence to the exact options that produced it.
    ///
    /// This consumes both values and returns [`DeviceActivationError::SessionOriginMismatch`]
    /// before connection when device identity, participant, activation, or
    /// successful session-key evidence differs from the originating activation attempt.
    pub fn into_connect_options(
        self,
        session: DeviceActivationSession,
    ) -> Result<DeviceConnectOptions<'a, C>, DeviceActivationError> {
        if self.connect.activation_origin_digest(
            self.activation_key_base64url,
            &self.nonce,
            &session.session_key_seed_base64url,
        ) != session.origin_digest
        {
            return Err(DeviceActivationError::SessionOriginMismatch);
        }
        Ok(self.connect)
    }
}

/// Current result of a device activation bootstrap attempt.
#[derive(Debug)]
pub enum DeviceActivationStatus {
    /// User or administrative activation work remains pending.
    Pending(DeviceActivationPending),
    /// Activation is complete; a subsequent pure device connection may proceed.
    Ready(DeviceActivationSession),
}

/// Durable local evidence needed to resume one pending activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceActivationPending {
    /// Server-owned activation review identifier.
    pub review_id: String,
    /// Portal URL for the activating user.
    pub activation_url: String,
    /// Device-local challenge nonce retained across attempts.
    pub nonce: String,
    /// Human confirmation code derived from the device-local activation key.
    pub confirmation_code: String,
    /// Server timestamp after which this review is no longer valid.
    pub expires_at: i64,
    /// Server time observed with the pending review.
    pub server_now: i64,
    /// Server-suggested polling delay.
    pub retry_after_ms: u64,
    /// Session signing seed retained across polling attempts.
    pub session_key_seed_base64url: String,
    /// Logical runtime connection retained across polling attempts.
    pub connection_id: String,
}

/// Evidence that the one-time device enrollment workflow reached ready state.
///
/// This value retains the successful attempt's private session seed
/// until [`DeviceActivationOptions::into_connect_options`] consumes it. Its [`Debug`] output is
/// deliberately redacted and never exposes either secret.
pub struct DeviceActivationSession {
    /// Server time observed in the ready bootstrap response.
    pub server_now: i64,
    session_key_seed_base64url: String,
    origin_digest: [u8; 32],
}

impl fmt::Debug for DeviceActivationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceActivationSession")
            .field("server_now", &self.server_now)
            .finish_non_exhaustive()
    }
}

/// Typed device activation failure.
#[derive(Debug, thiserror::Error)]
pub enum DeviceActivationError {
    /// Device or deployment was disabled.
    #[error("device activation is disabled")]
    Disabled,
    /// The pending activation review expired.
    #[error("device activation expired")]
    Expired,
    /// Device activation was rejected.
    #[error("device activation was rejected")]
    Rejected,
    /// The bounded activation wait elapsed.
    #[error("timed out waiting for device activation")]
    TimedOut,
    /// Runtime returned a state not valid for device activation.
    #[error("unexpected device activation state '{0}'")]
    UnexpectedState(String),
    /// Proof construction, HTTP, or response decoding failed.
    #[error(transparent)]
    Client(#[from] TrellisClientError),
    /// Device-local activation evidence was malformed.
    #[error("invalid device activation evidence: {0}")]
    InvalidEvidence(String),
    /// Ready evidence was paired with different device activation options.
    #[error("device activation session does not match its originating options")]
    SessionOriginMismatch,
}

/// Derive the eight-character confirmation code for one activation nonce.
pub fn derive_device_confirmation_code(
    activation_key_base64url: &str,
    public_identity_key: &str,
    nonce: &str,
) -> Result<String, DeviceActivationError> {
    let activation_key = URL_SAFE_NO_PAD
        .decode(activation_key_base64url)
        .map_err(|error| DeviceActivationError::InvalidEvidence(error.to_string()))?;
    let mut mac = HmacSha256::new_from_slice(&activation_key)
        .map_err(|error| DeviceActivationError::InvalidEvidence(error.to_string()))?;
    mac.update(DEVICE_CONFIRMATION_DOMAIN.as_bytes());
    mac.update(public_identity_key.as_bytes());
    mac.update(nonce.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut value = 0u64;
    for byte in &bytes[..5] {
        value = (value << 8) | u64::from(*byte);
    }
    Ok((0..8)
        .rev()
        .map(|shift| CROCKFORD_ALPHABET[((value >> (shift * 5)) & 31) as usize] as char)
        .collect())
}

/// Submit one fresh proof-bound device enrollment request.
pub async fn check_device_activation<C: crate::generated::ParticipantDescriptor>(
    options: &DeviceActivationOptions<'_, C>,
) -> Result<DeviceActivationStatus, DeviceActivationError> {
    check_device_activation_with_attempt(options, &DeviceActivationAttempt::new()?).await
}

async fn check_device_activation_with_attempt<C: crate::generated::ParticipantDescriptor>(
    options: &DeviceActivationOptions<'_, C>,
    attempt: &DeviceActivationAttempt,
) -> Result<DeviceActivationStatus, DeviceActivationError> {
    let confirmation_code = derive_device_confirmation_code(
        options.activation_key_base64url,
        &options.connect.public_identity_key()?,
        &options.nonce,
    )?;
    let challenge_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(options.nonce.as_bytes()));
    let response = match fetch_device_activation(
        &options.connect,
        &attempt.session_auth,
        &attempt.connection_id,
        options.provisioning_secret,
        &challenge_digest,
        &confirmation_code,
    )
    .await
    {
        Ok(response) => response,
        Err(TrellisClientError::BootstrapHttp { status: 403, .. }) => {
            return Err(DeviceActivationError::Disabled)
        }
        Err(error) => return Err(error.into()),
    };
    activation_status(options, confirmation_code, response, attempt)
}

fn activation_status<C>(
    options: &DeviceActivationOptions<'_, C>,
    confirmation_code: String,
    response: crate::client::DeviceEnrollmentResponse,
    attempt: &DeviceActivationAttempt,
) -> Result<DeviceActivationStatus, DeviceActivationError> {
    match response.state.as_str() {
        "approved" => Ok(DeviceActivationStatus::Ready(DeviceActivationSession {
            server_now: response.server_now,
            origin_digest: options.connect.activation_origin_digest(
                options.activation_key_base64url,
                &options.nonce,
                &attempt.session_key_seed_base64url,
            ),
            session_key_seed_base64url: attempt.session_key_seed_base64url.clone(),
        })),
        "pending" => {
            let activation = response.activation.ok_or_else(|| {
                DeviceActivationError::UnexpectedState("pending without review".into())
            })?;
            if activation.state != "pending" {
                return Err(DeviceActivationError::UnexpectedState(activation.state));
            }
            Ok(DeviceActivationStatus::Pending(DeviceActivationPending {
                review_id: activation.review_id,
                activation_url: activation.activation_url,
                nonce: options.nonce.clone(),
                confirmation_code,
                expires_at: activation.expires_at,
                server_now: response.server_now,
                retry_after_ms: activation.retry_after_ms,
                session_key_seed_base64url: attempt.session_key_seed_base64url.clone(),
                connection_id: attempt.connection_id.clone(),
            }))
        }
        "rejected" => Err(DeviceActivationError::Rejected),
        state => Err(DeviceActivationError::UnexpectedState(state.to_owned())),
    }
}

/// Poll current device activation with fresh request identities and proofs.
pub async fn wait_for_device_activation<C: crate::generated::ParticipantDescriptor>(
    options: &DeviceActivationOptions<'_, C>,
    pending: &DeviceActivationPending,
    timeout: Duration,
) -> Result<DeviceActivationSession, DeviceActivationError> {
    if options.nonce != pending.nonce {
        return Err(DeviceActivationError::InvalidEvidence(
            "pending nonce does not match activation options".into(),
        ));
    }
    let started_at = tokio::time::Instant::now();
    let deadline = started_at + timeout;
    let review_lifetime_ms = pending.expires_at.saturating_sub(pending.server_now);
    let review_deadline =
        started_at + Duration::from_millis(u64::try_from(review_lifetime_ms).unwrap_or_default());
    let mut attempt =
        DeviceActivationAttempt::from_seed(pending.session_key_seed_base64url.clone())?;
    attempt.connection_id.clone_from(&pending.connection_id);
    loop {
        let now = tokio::time::Instant::now();
        if now >= review_deadline {
            return Err(DeviceActivationError::Expired);
        }
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            return Err(DeviceActivationError::TimedOut);
        }
        tokio::time::sleep(
            Duration::from_millis(pending.retry_after_ms.max(1))
                .min(remaining)
                .min(review_deadline.saturating_duration_since(now)),
        )
        .await;
        match check_device_activation_with_attempt(options, &attempt).await {
            Err(DeviceActivationError::Disabled) => return Err(DeviceActivationError::Disabled),
            Err(DeviceActivationError::Rejected) => return Err(DeviceActivationError::Rejected),
            Err(error) => return Err(error),
            Ok(DeviceActivationStatus::Ready(session)) => return Ok(session),
            Ok(DeviceActivationStatus::Pending(current))
                if current.review_id == pending.review_id => {}
            Ok(DeviceActivationStatus::Pending(_)) => return Err(DeviceActivationError::Expired),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{derive_device_confirmation_code, DeviceActivationAttempt};

    #[test]
    fn confirmation_code_is_deterministic_and_crockford_encoded() {
        let code = derive_device_confirmation_code(
            "z89beQNUvhI08xF7ceiwvCD_kUF_RtBGcvDFsyiErgA",
            "PJOPafbG8Sq47Ra0sOSYmG2pJQj5FRgPrlwynA5Dq0I",
            "nonce",
        )
        .expect("confirmation code");
        assert_eq!(code, "HMHF9MKZ");
        assert_eq!(code.len(), 8);
        assert!(code
            .bytes()
            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte)));
        assert_eq!(
            code,
            derive_device_confirmation_code(
                "z89beQNUvhI08xF7ceiwvCD_kUF_RtBGcvDFsyiErgA",
                "PJOPafbG8Sq47Ra0sOSYmG2pJQj5FRgPrlwynA5Dq0I",
                "nonce",
            )
            .expect("same confirmation code")
        );
    }

    #[test]
    fn production_attempts_generate_distinct_session_keys() {
        let first = DeviceActivationAttempt::new().expect("first attempt");
        let second = DeviceActivationAttempt::new().expect("second attempt");
        assert_ne!(
            first.session_auth.session_key,
            second.session_auth.session_key
        );
    }
}
