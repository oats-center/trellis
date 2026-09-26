//! Authorization trust, context issuance, registry, and refresh runtime.

mod issuer;
mod registry;
mod repository;
pub(crate) mod trust;

pub(crate) use issuer::{AuthorizationContextIssueRequest, AuthorizationContextService};
pub(crate) use registry::{
    AuthorizationContextBundle, AuthorizationContextRegistry, AuthorizationRegistryBinding,
};
pub(in crate::platform::auth) use repository::load_sql_context_by_digest;
pub(crate) use repository::{
    context_revocation_action_id, revoke_sql_contexts, revoke_sql_contexts_matching,
    AuthorizationContextCommit, AuthorizationContextRecord, AuthorizationContextRepository,
    AuthorizationContextRevocationReason, AuthorizationContextSelector, AuthorizationContextState,
};
