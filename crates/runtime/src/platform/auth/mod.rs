//! Authoritative capability policy, catalog acceptance, and logical sessions.
//!
//! All signing and policy mutations share the platform SQLite transaction.
//! Broker effects are durable outbox work; no distributed policy ancestry is
//! required to verify the resulting authority.

mod administration;
pub(crate) use administration::Administration;
mod authorization_sessions;
mod broker;
mod catalog;
mod deployments;
mod framework_storage;
mod initialization;
pub(crate) use initialization::{console_registration, CONSOLE_CLIENT_ID};
mod lifecycle;
mod native_projection;
mod oauth;
mod own_context;
pub(crate) use own_context::OwnContextService;
mod avatars;
mod policy;
mod portal_themes;
mod profiles;
mod remote_http;
mod resources;
mod state;
pub(crate) use state::StateDeclaration;
mod revocation;
mod rpc;
mod rpc_credentials;
mod rpc_deployments;
mod rpc_policy;
mod rpc_policy_aggregates;
mod rpc_resource_bindings;
mod rpc_resources;
mod rpc_users;
mod service_updates;
mod sign_in;
pub(crate) use resources::materialize_native_kv;
pub(crate) use rpc::register_rpc;
mod sqlite;
mod telemetry;
pub(crate) use telemetry::AuthorizationTelemetrySnapshot;
mod transport;

pub(crate) use authorization_sessions::{AdmissionIdentity, Credential};
pub(crate) use broker::{MATERIAL_BUCKET, REVOCATION_BUCKET};
#[cfg(test)]
use catalog::ApiAcceptance;
pub(crate) use deployments::{DeploymentProvisioning, ProvisionedDeployment};
pub(crate) use lifecycle::{ClientRegistration, GrantApproval, VerifiedLogin};
pub(crate) use oauth::router as oauth_router;
pub(crate) use oauth::under_development_path;
pub(crate) use oauth::{OAuthEngine, OAuthListener, OAuthStorage};
pub(crate) use policy::{CapabilitySelection, PolicyMutation};
pub(crate) use revocation::EnforcementScope;
#[cfg(test)]
use sqlite::AuthSettings;
pub(crate) use sqlite::{blocking, read_custody_key};
pub(crate) use sqlite::{AuthError, Mutation, SqliteAuthorizationStore};

use crate::config::warn_oauth_cookie_isolation;

#[cfg(test)]
mod tests;
