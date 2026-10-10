//! Auth-owned public verification records. Decoding is not trust: consumers
//! authenticate the issuer, instance/account, and exact signed bytes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey};
use sha2::{Digest as _, Sha256};

use crate::authorization::{authorization_error, signed_json_digest};
use crate::AuthorizationErrorCode;
use crate::{
    canonicalize_json, ApiSurfaceKind, AuthorityIssuerKey, AuthorityIssuerState, PermissionAction,
    ProtocolError, SignedSessionAuthority, U64s,
};

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

fn metadata_error() -> ProtocolError {
    authorization_error(
        AuthorizationErrorCode::InvalidFormat,
        [],
        "invalid signed verification material",
    )
}

fn metadata_digest(record: &impl Serialize, domain: &str) -> Result<[u8; 32], ProtocolError> {
    let mut value = serde_json::to_value(record)?;
    let fields = value.as_object_mut().ok_or_else(metadata_error)?;
    fields.remove("signature");
    signed_json_digest(domain, &value)
}

fn verify_metadata(
    record: &impl Serialize,
    domain: &str,
    signature: &str,
    issuer: &AuthorityIssuerKey,
    instance: &str,
    account: &str,
) -> Result<(), ProtocolError> {
    let value = serde_json::to_value(record)?;
    // These records are bounded cold-path material, not a per-message policy graph.
    if canonicalize_json(&value)?.len() > 1_048_576
        || value["format"] != domain
        || value["trellisInstanceId"] != instance
        || value["audienceNatsAccount"] != account
        || value["critical"]
            .as_array()
            .is_none_or(|items| !items.is_empty())
        || issuer.state == AuthorityIssuerState::Revoked
        || value
            .get("issuerKeyId")
            .is_some_and(|id| id != &Value::String(issuer.key_id.clone()))
    {
        return Err(metadata_error());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| metadata_error())?;
    if URL_SAFE_NO_PAD.encode(&bytes) != signature {
        return Err(metadata_error());
    }
    let signature = Signature::from_slice(&bytes).map_err(|_| metadata_error())?;
    issuer
        .verifying_key()?
        .verify_strict(&metadata_digest(record, domain)?, &signature)
        .map_err(|_| metadata_error())
}

macro_rules! signed_record {
    ($record:ty, $domain:expr) => {
        impl $record {
            /// Sign Auth-owned verification material with the configured issuer.
            pub fn sign(mut self, key: &SigningKey) -> Result<Self, ProtocolError> {
                self.signature =
                    URL_SAFE_NO_PAD.encode(key.sign(&metadata_digest(&self, $domain)?).to_bytes());
                Ok(self)
            }
            /// Content address of the immutable, complete signed record.
            pub fn digest(&self) -> Result<String, ProtocolError> {
                Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(
                    canonicalize_json(&serde_json::to_value(self)?)?.as_bytes(),
                )))
            }
        }
    };
}
signed_record!(SignedCatalogSnapshot, CATALOG_SNAPSHOT_FORMAT_V1);
signed_record!(SignedProviderCertificate, PROVIDER_CERTIFICATE_FORMAT_V1);
signed_record!(SignedIssuerRotation, ISSUER_ROTATION_FORMAT_V1);
signed_record!(SignedSessionRevocation, SESSION_REVOCATION_FORMAT_V1);

impl SignedCatalogSnapshot {
    /// Authenticate an accepted catalog against pinned instance/account trust.
    pub fn verify(
        &self,
        issuer: &AuthorityIssuerKey,
        instance: &str,
        account: &str,
    ) -> Result<(), ProtocolError> {
        verify_metadata(
            self,
            CATALOG_SNAPSHOT_FORMAT_V1,
            &self.signature,
            issuer,
            instance,
            account,
        )?;
        if self.generation.get() == 0
            || self.accepted_revision.get() == 0
            || self.actions.iter().any(|action| {
                action.introduced_revision.get() == 0
                    || action.introduced_revision > self.accepted_revision
            })
            || self.capabilities.iter().any(|capability| {
                capability.identity_generation.get() == 0
                    || capability.consent_revision.get() == 0
                    || capability.memberships.iter().any(|member| {
                        member.member_since_revision.get() == 0
                            || member.member_since_revision > self.accepted_revision
                            || !self
                                .actions
                                .iter()
                                .any(|action| action.identity == member.action)
                    })
            })
        {
            return Err(metadata_error());
        }
        Ok(())
    }

    /// Check an exact action against previously verified authority and catalog.
    /// Both signatures, live time and logical-session revocation must be checked
    /// by the caller before this membership check can authorize dispatch.
    pub fn authorizes(
        &self,
        authority: &SignedSessionAuthority,
        action: &CatalogActionIdentity,
    ) -> bool {
        let Some(api) = authority
            .unsigned
            .apis
            .iter()
            .find(|api| api.api_id == self.api_id)
        else {
            return false;
        };
        if api.generation != self.generation || api.accepted_revision > self.accepted_revision {
            return false;
        }
        let Some(action_entry) = self.actions.iter().find(|entry| &entry.identity == action) else {
            return false;
        };
        api.accepted_revision >= action_entry.introduced_revision
            && self.capabilities.iter().any(|capability| {
                authority.unsigned.capabilities.iter().any(|grant| {
                    grant.capability_id == capability.capability_id
                        && grant.identity_generation == capability.identity_generation
                        && grant.consent_revision == capability.consent_revision
                }) && capability.memberships.iter().any(|member| {
                    &member.action == action
                        && api.accepted_revision >= member.member_since_revision
                })
            })
    }
}

impl SignedProviderCertificate {
    /// Verify provider identity, scope, signature and live validity.
    pub fn verify(
        &self,
        issuer: &AuthorityIssuerKey,
        instance: &str,
        account: &str,
        now: i64,
    ) -> Result<(), ProtocolError> {
        verify_metadata(
            self,
            PROVIDER_CERTIFICATE_FORMAT_V1,
            &self.signature,
            issuer,
            instance,
            account,
        )?;
        if self.generation.get() == 0
            || self.not_before > self.issued_at
            || self.issued_at >= self.expires_at
            || now < self.not_before
            || now >= self.expires_at
        {
            return Err(metadata_error());
        }
        Ok(())
    }
}

impl SignedSessionRevocation {
    /// Verify a sticky logical-session cutoff; an unsigned mirror is never a deny.
    pub fn verify(
        &self,
        issuer: &AuthorityIssuerKey,
        instance: &str,
        account: &str,
    ) -> Result<(), ProtocolError> {
        verify_metadata(
            self,
            SESSION_REVOCATION_FORMAT_V1,
            &self.signature,
            issuer,
            instance,
            account,
        )?;
        if ulid::Ulid::from_string(&self.authorization_session_id).is_err()
            || self.effective_cutoff < 0
            || self.issued_at < self.effective_cutoff
            || self.latest_context_expiry < self.effective_cutoff
        {
            return Err(metadata_error());
        }
        Ok(())
    }
}

impl SignedIssuerRotation {
    /// Install exactly the next authenticated rotation, never an older chain head.
    pub fn verify_successor(
        &self,
        previous: &AuthorityIssuerKey,
        instance: &str,
        account: &str,
        installed_sequence: u64,
        now: i64,
    ) -> Result<AuthorityIssuerKey, ProtocolError> {
        verify_metadata(
            self,
            ISSUER_ROTATION_FORMAT_V1,
            &self.signature,
            previous,
            instance,
            account,
        )?;
        if self.previous_key_id != previous.key_id
            || self.sequence.get()
                != installed_sequence
                    .checked_add(1)
                    .ok_or_else(metadata_error)?
            || self.activated_at > now
            || self.previous_retired_at < self.activated_at
        {
            return Err(metadata_error());
        }
        let next = AuthorityIssuerKey {
            key_id: self.next_key_id.clone(),
            public_key: self.next_public_key.clone(),
            state: AuthorityIssuerState::Active,
        };
        next.verifying_key()?;
        if next.key_id == previous.key_id {
            return Err(metadata_error());
        }
        Ok(next)
    }
}
