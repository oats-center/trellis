use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client-side verification and refresh limits from the authenticated server.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationContextPolicy {
    /// Symmetric clock skew accepted by the issuer.
    pub allowed_clock_skew_seconds: u32,
    /// Maximum context lease duration.
    pub maximum_context_lifetime_seconds: u32,
    /// Maximum canonical signed-context JSON size in UTF-8 bytes.
    pub maximum_context_bytes: usize,
    /// Maximum exact permission atoms.
    pub maximum_permissions: usize,
    /// Safety lead before expiry used for proactive refresh.
    pub refresh_lead_seconds: u32,
    /// Deterministic earlier-only refresh jitter window.
    pub refresh_jitter_seconds: u32,
}

impl AuthorizationContextPolicy {
    /// Construct the shared protocol policy at the server-corrected current time.
    pub fn verification_policy(
        &self,
        now: i64,
    ) -> Result<trellis_protocol::AuthorizationVerificationPolicy, trellis_protocol::ProtocolError>
    {
        trellis_protocol::AuthorizationVerificationPolicy::new(
            now,
            self.allowed_clock_skew_seconds,
            self.maximum_context_lifetime_seconds,
            self.maximum_context_bytes,
            self.maximum_permissions,
        )
    }
}

pub(crate) enum AuthorizationCredential {
    Native {
        kind: trellis_protocol::AuthorizationPrincipalKind,
        identity: std::sync::Arc<super::super::SessionAuth>,
        package_evidence: crate::generated::PackageEvidence,
        participant_path: &'static str,
        companion: Option<NativeCompanionCredential>,
    },
    User {
        login_session_id: String,
        installation: std::sync::Arc<super::super::SessionAuth>,
    },
}

pub(crate) struct NativeCompanionCredential {
    pub(crate) participant_id: &'static str,
    pub(crate) installation: std::sync::Arc<super::super::SessionAuth>,
}

/// NATS-backed context and revocation registry binding from the server.
///
/// The binding is internal runtime/SDK material: service authors never receive
/// raw registry handles or subject names.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationRegistryBinding {
    /// KV bucket holding contexts and revocations.
    pub context_bucket: String,
}

#[cfg(feature = "runtime-internals")]
impl AuthorizationRegistryBinding {
    #[doc(hidden)]
    #[must_use]
    pub fn from_runtime_parts(context_bucket: String) -> Self {
        Self { context_bucket }
    }
}

/// Signed context and its authenticated online issuer and runtime metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationContextBundle {
    /// Complete signed authorization context.
    pub context: Value,
    /// Current issuer entry received from the configured origin.
    pub issuer: trellis_protocol::AuthorizationIssuerKey,
    /// NATS-backed authorization evidence registry binding.
    pub(crate) authorization_registry: AuthorizationRegistryBinding,
    /// Verification and refresh policy for this runtime.
    pub policy: AuthorizationContextPolicy,
}

#[cfg(feature = "runtime-internals")]
impl AuthorizationContextBundle {
    /// Assemble server-issued wire metadata; clients still verify it at installation.
    #[doc(hidden)]
    pub fn from_runtime_parts(
        context: Value,
        issuer: trellis_protocol::AuthorizationIssuerKey,
        authorization_registry: AuthorizationRegistryBinding,
        policy: AuthorizationContextPolicy,
    ) -> Self {
        Self {
            context,
            issuer,
            authorization_registry,
            policy,
        }
    }
}

/// Route-selection JWT installed atomically with an authorization context.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationRoutingMaterial {
    /// Deny-all Auth-account JWT used only to select the Auth Callout route.
    pub bootstrap_jwt: String,
    /// JWT expiry as Unix seconds.
    pub bootstrap_jwt_expires_at: i64,
}

/// Native transport endpoints installed with one authorization context.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationNativeTransport {
    /// Current native NATS endpoints.
    pub nats_servers: Vec<String>,
}

/// Typed transports installed with one authorization context.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationRuntimeTransports {
    /// Native transport endpoints, when offered.
    pub native: Option<AuthorizationNativeTransport>,
    /// WebSocket transport endpoints, when offered.
    pub websocket: Option<AuthorizationNativeTransport>,
}

/// Proof-bound assignment and transport metadata for one runtime connection.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationRuntimeBinding {
    /// SDK-owned connection identifier.
    pub connection_id: String,
    /// Durable user login identifier, absent for native credentials.
    pub login_session_id: Option<String>,
    /// Stable participant identifier.
    pub participant_id: String,
    /// Current NATS reply-inbox prefix.
    pub inbox_prefix: String,
    /// Current typed runtime transports.
    pub transports: AuthorizationRuntimeTransports,
}

/// Reserved provider deployment selection for one API.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationApiBinding {
    /// Deployment selected to provide the API.
    pub provider_deployment_id: String,
}

/// One complete authorization/runtime installation committed atomically.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationInstallation {
    /// Signed authorization context and trust evidence.
    pub context: AuthorizationContextBundle,
    /// Route JWT paired with the context.
    pub routing: AuthorizationRoutingMaterial,
    /// Proof-bound session and runtime connection metadata.
    pub runtime: AuthorizationRuntimeBinding,
    /// API provider selections reserved for deployment routing.
    pub api_bindings: std::collections::BTreeMap<String, AuthorizationApiBinding>,
    /// Server-clock correction in milliseconds.
    pub server_clock_offset_ms: i64,
    /// Server-owned native resource evidence, absent for user connections.
    pub(crate) authorization: Option<Value>,
}

/// Verified current-context material held by the own-context cache.
#[derive(Clone, Debug)]
pub(crate) struct CurrentContext {
    pub(crate) bundle: AuthorizationContextBundle,
    pub(crate) context_digest: String,
    pub(crate) not_before: i64,
    pub(crate) expires_at: i64,
    pub(crate) refresh_at: i64,
}

/// In-process own-context state.
#[derive(Clone, Debug, Default)]
pub(crate) struct CachedAuthorizationState {
    pub(crate) current: Option<CurrentContext>,
    pub(crate) runtime: Option<AuthorizationRuntimeBinding>,
    pub(crate) routing: Option<AuthorizationRoutingMaterial>,
    pub(crate) api_bindings: std::collections::BTreeMap<String, AuthorizationApiBinding>,
    pub(crate) server_clock_offset_ms: i64,
    pub(crate) authorization: Option<Value>,
}
