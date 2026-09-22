use std::sync::Arc;

use super::super::account::hash_password;
use super::super::AuthorizationStateError;
use super::activation_review_notifier::ActivationReviewNotifier;

/// Security settings for the Rust-owned authentication service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthServiceConfig {
    /// Minimum local-password length in Unicode scalar values.
    pub password_min_length: usize,
    /// Consecutive failures that trigger a local-account lock.
    pub maximum_login_failures: u32,
    /// Local-account lock duration in milliseconds.
    pub login_lock_duration_ms: u64,
    /// Lifetime of a one-time first-administrator flow in milliseconds.
    pub first_admin_flow_ttl_ms: u64,
    /// Default authenticated-session lifetime in milliseconds.
    pub session_ttl_ms: u64,
    /// Lifetime of a one-time device provisioning secret in milliseconds.
    pub device_provisioning_secret_ttl_ms: u64,
}

impl Default for AuthServiceConfig {
    fn default() -> Self {
        Self {
            password_min_length: 12,
            maximum_login_failures: 5,
            login_lock_duration_ms: 15 * 60_000,
            first_admin_flow_ttl_ms: 24 * 60 * 60_000,
            session_ttl_ms: 24 * 60 * 60_000,
            device_provisioning_secret_ttl_ms: 15 * 60_000,
        }
    }
}

/// Single Rust-owned composition root for auth domain behavior.
#[derive(Clone, Debug)]
pub struct AuthService<R> {
    pub(super) repository: R,
    pub(super) config: AuthServiceConfig,
    pub(super) dummy_password_hash: Arc<str>,
    pub(super) activation_reviews: ActivationReviewNotifier,
}

impl<R> AuthService<R>
where
    R: Clone,
{
    /// Construct auth behavior over one coherent repository set.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationStateError::InvalidRecord`] for unsafe password
    /// or lockout settings, or a storage error if the uniform-failure hash
    /// cannot be generated.
    pub(crate) fn new(
        repository: R,
        config: AuthServiceConfig,
    ) -> Result<Self, AuthorizationStateError> {
        if config.maximum_login_failures == 0
            || config.login_lock_duration_ms == 0
            || config.first_admin_flow_ttl_ms == 0
            || config.session_ttl_ms == 0
            || config.device_provisioning_secret_ttl_ms == 0
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "local login lockout limits must be positive".to_owned(),
            ));
        }
        let (dummy_password_hash, _) = hash_password(
            "trellis uniform local authentication failure",
            Some(config.password_min_length),
        )?;
        Ok(Self {
            repository,
            config,
            dummy_password_hash: dummy_password_hash.into(),
            activation_reviews: ActivationReviewNotifier,
        })
    }

    /// Borrow the coherent auth repository set.
    #[must_use]
    pub(crate) fn repository(&self) -> &R {
        &self.repository
    }

    /// Return the configured minimum local-password length.
    #[must_use]
    pub(crate) fn password_min_length(&self) -> usize {
        self.config.password_min_length
    }
}
