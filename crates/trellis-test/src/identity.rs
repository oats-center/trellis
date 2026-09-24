//! Identity results returned to callers.
//!
//! Identity values describe how to connect an application-owned generated
//! client or service; they do not own the transport. The application connects
//! with its generated facade, then stops that transport before shutting down
//! the test runtime. Secrets are never printed by `Debug`.

use std::fmt;

use trellis_rs::client::{UserConnectOptions, UserSessionCredentials};
use trellis_rs::service::ServiceConnectOptions;

/// A participant installed into the runtime's fresh platform.
#[derive(Clone, Debug)]
pub struct InstalledParticipant {
    pub(crate) participant_id: String,
    pub(crate) installed_revision: u64,
}

impl InstalledParticipant {
    /// Qualified participant identity.
    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    /// Server-confirmed installed revision.
    #[must_use]
    pub fn installed_revision(&self) -> u64 {
        self.installed_revision
    }
}

/// A provisioned service identity and the options its service connects with.
pub struct TestServiceIdentity {
    pub(crate) name: String,
    pub(crate) participant_id: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: String,
    pub(crate) seed: String,
    pub(crate) trellis_url: String,
    pub(crate) timeout_ms: u64,
}

impl TestServiceIdentity {
    /// Registration name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Qualified participant identity.
    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    /// Deployment created for this registration.
    #[must_use]
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    /// Provisioned service instance id.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Base64url provisioned identity seed the service presents.
    #[must_use]
    pub fn seed(&self) -> &str {
        &self.seed
    }

    /// Options for the service's generated `connect` call.
    #[must_use]
    pub fn connect_options(&self) -> ServiceConnectOptions<'_> {
        ServiceConnectOptions::new(&self.trellis_url, &self.seed)
            .with_name(&self.name)
            .with_timeout_ms(self.timeout_ms)
    }
}

impl fmt::Debug for TestServiceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestServiceIdentity")
            .field("name", &self.name)
            .field("participant_id", &self.participant_id)
            .field("deployment_id", &self.deployment_id)
            .field("instance_id", &self.instance_id)
            .field("seed", &"[redacted]")
            .finish()
    }
}

/// A logged-in application or agent caller identity.
pub struct TestClientIdentity {
    pub(crate) name: String,
    pub(crate) participant_id: String,
    pub(crate) login_session_id: String,
    pub(crate) session_seed: String,
    pub(crate) trellis_url: String,
    pub(crate) timeout_ms: u64,
}

impl TestClientIdentity {
    /// Registration name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Qualified participant identity.
    #[must_use]
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }

    /// Durable login-session id issued by the proof-bound final bind.
    #[must_use]
    pub fn login_session_id(&self) -> &str {
        &self.login_session_id
    }

    /// Options for the caller's generated client `connect` call.
    #[must_use]
    pub fn connect_options(&self) -> UserConnectOptions<'_> {
        UserConnectOptions::new(
            &self.trellis_url,
            self.timeout_ms,
            UserSessionCredentials {
                login_session_id: &self.login_session_id,
                session_key_seed_base64url: &self.session_seed,
            },
            &self.participant_id,
        )
        .with_name(&self.name)
    }
}

impl fmt::Debug for TestClientIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestClientIdentity")
            .field("name", &self.name)
            .field("participant_id", &self.participant_id)
            .field("login_session_id", &self.login_session_id)
            .field("session_seed", &"[redacted]")
            .finish()
    }
}
