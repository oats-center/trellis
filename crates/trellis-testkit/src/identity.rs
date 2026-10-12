//! Identity results returned to callers.
//!
//! Identity values describe how to connect an application-owned generated
//! client or service; they do not own the transport. The application connects
//! with its generated facade, then stops that transport before shutting down
//! the test runtime. Secrets are never printed by `Debug`.

use std::fmt;

use trellis_rs::auth::NativeOAuthSession;
use trellis_rs::client::{RuntimeAuthorizationTrust, UserConnectOptions};
use trellis_rs::service::ServiceConnectOptions;

/// A provisioned service identity and the options its service connects with.
pub struct TestServiceIdentity {
    pub(crate) name: String,
    pub(crate) participant_id: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: String,
    pub(crate) seed: String,
    pub(crate) trellis_url: String,
    pub(crate) nats_servers: Vec<String>,
    pub(crate) trust: RuntimeAuthorizationTrust,
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

    /// Base URL of the runtime this identity was provisioned in.
    #[must_use]
    pub fn trellis_url(&self) -> &str {
        &self.trellis_url
    }

    /// Options for the service's generated `connect` call.
    #[must_use]
    pub fn connect_options(&self) -> ServiceConnectOptions<'_> {
        ServiceConnectOptions::new(self.nats_servers.clone(), self.trust.clone(), &self.seed)
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
    pub(crate) client_id: String,
    pub(crate) session: NativeOAuthSession,
    pub(crate) nats_servers: Vec<String>,
    pub(crate) trellis_url: String,
    pub(crate) timeout_ms: u64,
}

impl TestClientIdentity {
    /// Registration name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Registered OAuth client identity, independent of source application requests.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Durable login-session id from the issuer-verified OAuth admission.
    #[must_use]
    pub fn login_session_id(&self) -> &str {
        &self.session.admission.credential.login_session_id
    }

    /// Base URL of the runtime this identity was bound in.
    #[must_use]
    pub fn trellis_url(&self) -> &str {
        &self.trellis_url
    }

    /// Consume exclusive OAuth custody for the caller's generated `connect` call.
    #[must_use]
    pub fn connect_options(self) -> UserConnectOptions<'static> {
        UserConnectOptions::new(self.nats_servers, self.timeout_ms, self.session)
    }
}

impl fmt::Debug for TestClientIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestClientIdentity")
            .field("name", &self.name)
            .field("client_id", &self.client_id)
            .field("login_session_id", &self.login_session_id())
            .finish()
    }
}
