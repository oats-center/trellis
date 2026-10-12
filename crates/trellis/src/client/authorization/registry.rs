//! Bounded public material reads. Trust, caching and revocations belong to the
//! connected authority owner, not to individual reads or per-context watchers.

use async_nats::jetstream;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use trellis_protocol::{
    parse_session_authority, SignedCatalogSnapshot, SignedIssuerRotation,
    SignedProviderCertificate, SignedSessionAuthority, SignedSessionRevocation,
    MAX_ADMISSION_RESPONSE_BYTES, MAX_SESSION_AUTHORITY_BYTES,
};

use super::super::TrellisClientError;

pub(crate) const MATERIAL_BUCKET: &str = "TRELLIS_AUTH_MATERIAL";
pub(crate) const REVOCATION_BUCKET: &str = "TRELLIS_AUTH_REVOCATIONS";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum MaterialKind {
    Authority,
    Catalog,
    Provider,
}

/// Immutable bytes decoded and content-address checked, but not issuer-trusted.
#[derive(Clone, Debug)]
pub(crate) enum RegistryMaterial {
    Authority(SignedSessionAuthority),
    Catalog(SignedCatalogSnapshot),
    Provider(SignedProviderCertificate),
}

impl RegistryMaterial {
    pub(crate) fn issuer_key_id(&self) -> &str {
        match self {
            Self::Authority(authority) => &authority.unsigned.issuer_key_id,
            Self::Catalog(catalog) => &catalog.issuer_key_id,
            Self::Provider(certificate) => &certificate.issuer_key_id,
        }
    }
}

/// Stateless network adapter. Each read uses the owner's leased attachment;
/// a resolver cannot accidentally retain a superseded physical connection.
#[derive(Clone, Debug, Default)]
pub(crate) struct AuthorizationRegistryReader;

impl AuthorizationRegistryReader {
    pub(crate) async fn material(
        client: &async_nats::Client,
        kind: MaterialKind,
        digest: &str,
    ) -> Result<RegistryMaterial, TrellisClientError> {
        let decoded = URL_SAFE_NO_PAD.decode(digest).map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("invalid material digest".into())
        })?;
        if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(decoded) != digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "invalid material digest".into(),
            ));
        }
        let (prefix, maximum_bytes) = match kind {
            MaterialKind::Authority => ("context", MAX_SESSION_AUTHORITY_BYTES),
            MaterialKind::Catalog => ("api", MAX_ADMISSION_RESPONSE_BYTES),
            MaterialKind::Provider => ("provider", MAX_ADMISSION_RESPONSE_BYTES),
        };
        let bytes = Self::read(
            client,
            MATERIAL_BUCKET,
            &format!("{prefix}.{digest}"),
            maximum_bytes,
        )
        .await?
        .ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable("verification material unavailable".into())
        })?;
        let material = match kind {
            MaterialKind::Authority => {
                RegistryMaterial::Authority(parse_session_authority(&bytes, maximum_bytes)?)
            }
            MaterialKind::Catalog => RegistryMaterial::Catalog(serde_json::from_slice(&bytes)?),
            MaterialKind::Provider => RegistryMaterial::Provider(serde_json::from_slice(&bytes)?),
        };
        let actual = match &material {
            RegistryMaterial::Authority(authority) => authority.digest()?,
            RegistryMaterial::Catalog(catalog) => catalog.digest()?,
            RegistryMaterial::Provider(certificate) => certificate.digest()?,
        };
        if actual != digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "verification material digest mismatch".into(),
            ));
        }
        Ok(material)
    }

    pub(crate) async fn rotation(
        client: &async_nats::Client,
        sequence: u64,
    ) -> Result<Option<SignedIssuerRotation>, TrellisClientError> {
        Self::read(
            client,
            MATERIAL_BUCKET,
            &format!("rotation.{sequence}"),
            MAX_SESSION_AUTHORITY_BYTES,
        )
        .await?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(TrellisClientError::from))
        .transpose()
    }

    pub(crate) async fn revocation(
        client: &async_nats::Client,
        session_id: &str,
    ) -> Result<Option<SignedSessionRevocation>, TrellisClientError> {
        if ulid::Ulid::from_string(session_id).is_err() {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "invalid logical session".into(),
            ));
        }
        Self::read(
            client,
            REVOCATION_BUCKET,
            session_id,
            MAX_SESSION_AUTHORITY_BYTES,
        )
        .await?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(TrellisClientError::from))
        .transpose()
    }

    async fn read(
        client: &async_nats::Client,
        bucket: &str,
        key: &str,
        maximum_bytes: usize,
    ) -> Result<Option<Vec<u8>>, TrellisClientError> {
        let store = jetstream::new(client.clone())
            .get_key_value(bucket)
            .await
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "verification registry unavailable".into(),
                )
            })?;
        let value = store.get(key).await.map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("verification registry read failed".into())
        })?;
        match value {
            Some(value) if !value.is_empty() && value.len() <= maximum_bytes => {
                Ok(Some(value.to_vec()))
            }
            Some(_) => Err(TrellisClientError::AuthorizationUnavailable(
                "verification material exceeds size budget".into(),
            )),
            None => Ok(None),
        }
    }
}
