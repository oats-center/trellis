use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use super::AuthorizationStateError;

mod account_flows;
mod accounts;
mod activation_review_notifier;
mod deployments;
pub(crate) mod repository;
mod service;
mod sessions;
pub(crate) mod validation;

pub use service::{AuthService, AuthServiceConfig};

pub use account_flows::{
    CompleteIdentityLinkInput, CreateAccountFlowInput, FirstAdminAuthorityTarget,
    FirstAdminBinding, FirstAdminFederatedRegistration, FirstAdminRegistration,
};
pub use accounts::{
    ChangePasswordInput, CompletePasswordResetInput, CreateFederatedUserInput,
    CreateLocalUserInput, CreateUserInput, LocalAuthentication, UpdateUserInput, UserAccount,
};
pub use deployments::{
    ClaimActivationReviewInput, CreateActivationReviewInput, DecideActivationReviewInput,
    EnrollDeviceIdentityInput, ProvisionDeviceInput, ProvisionServiceIdentityInput,
};
pub use sessions::CreateSessionInput;

pub(crate) fn bearer_secret_digest(value: &str) -> Result<String, AuthorizationStateError> {
    let secret = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        AuthorizationStateError::InvalidRecord("secret is not canonical base64url".to_owned())
    })?;
    if secret.len() != 32 || URL_SAFE_NO_PAD.encode(&secret) != value {
        return Err(AuthorizationStateError::InvalidRecord(
            "secret must canonically encode 32 bytes".to_owned(),
        ));
    }
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(secret)))
}
