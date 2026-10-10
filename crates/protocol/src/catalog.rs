//! Auth-owned public verification records. Decoding is not trust: consumers
//! authenticate the issuer, instance/account, and exact signed bytes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{ApiSurfaceKind, PermissionAction, U64s};

/// Accepted catalog format and signature domain.
pub const CATALOG_SNAPSHOT_FORMAT_V1: &str = "trellis.catalog-snapshot.v1";
/// Provider certificate format and signature domain.
pub const PROVIDER_CERTIFICATE_FORMAT_V1: &str = "trellis.provider-certificate.v1";
/// Authenticated issuer rotation format and signature domain.
pub const ISSUER_ROTATION_FORMAT_V1: &str = "trellis.issuer-rotation.v1";
/// Logical authorization-session revocation format and signature domain.
pub const SESSION_REVOCATION_FORMAT_V1: &str = "trellis.session-revocation.v1";

/// Stable compiler/dispatcher action identity, never an administrator grant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogActionIdentity {
    /// Declared API surface kind.
    pub kind: ApiSurfaceKind,
    /// Native action name, including its declared namespace.
    pub name: String,
    /// Direction or lifecycle operation.
    pub direction: PermissionAction,
}

/// Introduction of an action within one API generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogAction {
    /// Stable declared action.
    pub identity: CatalogActionIdentity,
    /// Acceptance revision that introduced or reintroduced the action.
    pub introduced_revision: U64s,
}

/// A whole capability's membership of an internal dispatcher action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogCapabilityMembership {
    /// Declared action.
    pub action: CatalogActionIdentity,
    /// Revision that introduced or reintroduced this membership.
    pub member_since_revision: U64s,
}

/// Accepted identity and meaning of a whole capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogCapability {
    /// Stable API-qualified capability ID.
    pub capability_id: String,
    /// Identity generation, independent of API force replacement.
    pub identity_generation: U64s,
    /// Explicit authored consent meaning revision.
    pub consent_revision: U64s,
    /// Internal action membership metadata.
    pub memberships: Vec<CatalogCapabilityMembership>,
}

/// Immutable signed source identities and deployment-local acceptance counters.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedCatalogSnapshot {
    /// Exact catalog format/signature domain.
    pub format: String,
    /// Issuer lookup ID, not a trust anchor by itself.
    pub issuer_key_id: String,
    /// Owning Trellis instance.
    pub trellis_instance_id: String,
    /// Intended NATS account.
    pub audience_nats_account: String,
    /// Version-qualified API ID.
    pub api_id: String,
    /// Active API generation assigned by Auth.
    pub generation: U64s,
    /// Accepted revision assigned by Auth.
    pub accepted_revision: U64s,
    /// Accepted canonical definition digest.
    pub definition_digest: String,
    /// Action introduction metadata.
    pub actions: Vec<CatalogAction>,
    /// Whole-capability identity, meaning, and membership metadata.
    pub capabilities: Vec<CatalogCapability>,
    /// Unix issuance time in seconds.
    pub issued_at: i64,
    /// Integrity-bound extensions.
    pub extensions: Map<String, Value>,
    /// Critical extension names.
    pub critical: Vec<String>,
    /// Canonical base64url Ed25519 issuer signature.
    pub signature: String,
}

/// Provider rights for a deployment/runtime's implemented API subset.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedProviderCertificate {
    /// Exact provider format/signature domain.
    pub format: String,
    /// Issuer lookup ID.
    pub issuer_key_id: String,
    /// Owning Trellis instance.
    pub trellis_instance_id: String,
    /// Intended NATS account.
    pub audience_nats_account: String,
    /// Provisioned principal.
    pub principal_id: String,
    /// Authorized deployment.
    pub deployment_id: String,
    /// Service/device instance.
    pub instance_id: String,
    /// Ephemeral provider message key.
    pub session_public_key: String,
    /// Implemented API ID.
    pub api_id: String,
    /// Implemented API generation.
    pub generation: U64s,
    /// Verified implementation semantics digest.
    pub implementation_digest: String,
    /// Implemented subset; caller capabilities confer no provider rights.
    pub implemented_actions: Vec<CatalogActionIdentity>,
    /// Unix issuance time in seconds.
    pub issued_at: i64,
    /// Inclusive lower acceptance bound.
    pub not_before: i64,
    /// Exclusive upper live acceptance bound.
    pub expires_at: i64,
    /// Integrity-bound extensions.
    pub extensions: Map<String, Value>,
    /// Critical extension names.
    pub critical: Vec<String>,
    /// Canonical base64url Ed25519 issuer signature.
    pub signature: String,
}

/// Rotation authenticated by the previously pinned issuer key.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedIssuerRotation {
    /// Exact rotation format/signature domain.
    pub format: String,
    /// Owning Trellis instance.
    pub trellis_instance_id: String,
    /// Intended NATS account.
    pub audience_nats_account: String,
    /// Monotonic authenticated chain position.
    pub sequence: U64s,
    /// Previously trusted signer.
    pub previous_key_id: String,
    /// Successor lookup ID.
    pub next_key_id: String,
    /// Canonical base64url successor Ed25519 public key.
    pub next_public_key: String,
    /// Unix successor activation time in seconds.
    pub activated_at: i64,
    /// Previous signer's ordinary retirement time.
    pub previous_retired_at: i64,
    /// Integrity-bound extensions.
    pub extensions: Map<String, Value>,
    /// Critical extension names.
    pub critical: Vec<String>,
    /// Previous key's Ed25519 signature.
    pub signature: String,
}

/// Immutable historical cutoff for one logical authorization-session ID.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedSessionRevocation {
    /// Exact revocation format/signature domain.
    pub format: String,
    /// Issuer lookup ID.
    pub issuer_key_id: String,
    /// Owning Trellis instance.
    pub trellis_instance_id: String,
    /// Intended NATS account.
    pub audience_nats_account: String,
    /// Irreversibly retired logical session.
    pub authorization_session_id: String,
    /// Exclusive historical publication acceptance cutoff.
    pub effective_cutoff: i64,
    /// Static machine-readable reason.
    pub reason: String,
    /// Unix statement issuance time in seconds.
    pub issued_at: i64,
    /// Final live deadline for hot mirror/cache retention.
    pub latest_context_expiry: i64,
    /// Integrity-bound extensions.
    pub extensions: Map<String, Value>,
    /// Critical extension names.
    pub critical: Vec<String>,
    /// Ed25519 issuer signature.
    pub signature: String,
}
