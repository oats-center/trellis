//! Authoritative capability policy, catalog acceptance, and logical sessions.
//!
//! All signing and policy mutations share the platform SQLite transaction.
//! Broker effects are durable outbox work; no distributed policy ancestry is
//! required to verify the resulting authority.

mod access_records;
mod administration;
pub(crate) use administration::Administration;
mod authorization_sessions;
mod broker;
mod catalog;
mod config;
mod deployments;
mod diagnostics;
pub(crate) use diagnostics::{register_rpc as register_diagnostics_rpc, Diagnostics};
mod installation_export;
mod instances;
pub(crate) use instances::STARTUP_QUIET_SECONDS;
mod framework_storage;
mod initialization;
pub(crate) use initialization::{console_registration, CONSOLE_CLIENT_ID};
mod lifecycle;
mod native_projection;
mod oauth;
mod own_context;
mod packages;
pub(crate) use own_context::OwnContextService;
mod avatars;
mod policy;
mod portal_themes;
mod profiles;
mod registries;
mod release_check;
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
mod service_bundles;
mod service_updates;
mod sign_in;
pub(crate) mod webhooks;
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
pub(crate) use installation_export::{
    export as export_installation, import as import_installation,
};
pub(crate) use lifecycle::{ClientRegistration, GrantApproval, VerifiedLogin};
pub(crate) use oauth::router as oauth_router;
pub(crate) use oauth::{OAuthEngine, OAuthStorage};
pub(crate) use policy::{CapabilitySelection, PolicyMutation};
pub(crate) use revocation::EnforcementScope;
#[cfg(test)]
use sqlite::AuthSettings;
pub(crate) use sqlite::{blocking, issuer_signing_key, oauth_storage_key};
pub(crate) use sqlite::{AuthError, Mutation, SqliteAuthorizationStore};

use crate::config::warn_oauth_cookie_isolation;

#[cfg(test)]
pub(crate) mod tests;
