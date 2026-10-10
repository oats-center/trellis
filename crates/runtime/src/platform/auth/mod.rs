//! Authoritative capability policy, catalog acceptance, and logical sessions.
//!
//! All signing and policy mutations share the platform SQLite transaction.
//! Broker effects are durable outbox work; no distributed policy ancestry is
//! required to verify the resulting authority.

mod authorization_sessions;
mod broker;
mod catalog;
mod deployments;
mod lifecycle;
mod policy;
mod revocation;
mod sqlite;

pub(crate) use authorization_sessions::{AdmissionIdentity, Credential, IssuedAuthority};
pub(crate) use catalog::{ApiAcceptance, ApiReview};
pub(crate) use deployments::{DeploymentProvisioning, ProvisionedDeployment};
pub(crate) use lifecycle::{ClientRegistration, GrantApproval, VerifiedLogin};
pub(crate) use policy::{CapabilitySelection, Evaluation, PolicyMutation};
pub(crate) use revocation::{Attachment, EnforcementScope, PendingEffect};
pub(crate) use sqlite::{AuthError, AuthSettings, Mutation, SqliteAuthorizationStore};

#[cfg(test)]
mod phase_two_tests;
