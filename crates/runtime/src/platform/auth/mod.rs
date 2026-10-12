//! Authoritative capability policy, catalog acceptance, and logical sessions.
//!
//! All signing and policy mutations share the platform SQLite transaction.
//! Broker effects are durable outbox work; no distributed policy ancestry is
//! required to verify the resulting authority.

mod authorization_sessions;
mod broker;
mod catalog;
mod deployments;
mod initialization;
pub(crate) use initialization::{console_registration, CONSOLE_CLIENT_ID};
mod lifecycle;
mod oauth;
mod own_context;
pub(crate) use own_context::OwnContextService;
mod policy;
mod resources;
mod revocation;
mod rpc;
mod rpc_credentials;
mod rpc_deployments;
mod rpc_policy;
mod rpc_policy_aggregates;
mod rpc_resources;
mod rpc_users;
pub(crate) use resources::materialize_native_kv;
pub(crate) use rpc::register_rpc;
mod sqlite;
mod telemetry;
pub(crate) use telemetry::AuthorizationTelemetrySnapshot;
mod transport;

pub(crate) use authorization_sessions::{AdmissionIdentity, Credential, IssuedAuthority};
pub(crate) use broker::{MATERIAL_BUCKET, REVOCATION_BUCKET};
pub(crate) use catalog::{ApiAcceptance, ApiReview};
pub(crate) use deployments::{DeploymentProvisioning, ProvisionedDeployment};
pub(crate) use lifecycle::{ClientRegistration, GrantApproval, VerifiedLogin};
pub(crate) use oauth::router as oauth_router;
pub(crate) use oauth::{OAuthEngine, OAuthListener, OAuthStorage};
pub(crate) use policy::{CapabilitySelection, Evaluation, PolicyMutation};
pub(crate) use revocation::{Attachment, EnforcementScope, PendingEffect};
pub(crate) use sqlite::read_custody_key;
pub(crate) use sqlite::{AuthError, AuthSettings, Mutation, SqliteAuthorizationStore};

use crate::config::warn_oauth_cookie_isolation;

#[cfg(test)]
mod phase_two_tests;
