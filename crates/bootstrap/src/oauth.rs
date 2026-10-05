use std::{collections::BTreeMap, path::Path};

use serde::Deserialize;
use trellis_runtime::OAuthProviderConfig;

use crate::BootstrapError;

/// Bootstrap JSON provider input, using service-author camelCase names.
/// Secrets are deliberately omitted from Debug and parse diagnostics.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapOAuthProvider {
    #[serde(rename = "type")]
    provider_type: ProviderType,
    issuer: Option<String>,
    client_id: String,
    client_secret: Option<String>,
    display_name: Option<String>,
    scopes: Option<Vec<String>>,
    #[serde(default)]
    role_claims: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProviderType {
    Github,
    Oidc,
}

impl std::fmt::Debug for BootstrapOAuthProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapOAuthProvider")
            .field("type", &self.provider_type)
            .finish_non_exhaustive()
    }
}

/// Parse a private JSON provider file before bootstrap output is mutated.
///
/// # Errors
/// Returns a redacted input error for malformed or unsupported provider settings.
pub fn read_oauth_providers(
    path: &Path,
) -> Result<BTreeMap<String, OAuthProviderConfig>, BootstrapError> {
    let bytes = std::fs::read(path)?;
    let providers: BTreeMap<String, BootstrapOAuthProvider> =
        serde_json::from_slice(&bytes).map_err(|_| BootstrapError::InvalidOAuthInput)?;
    let mut result = BTreeMap::new();
    for (id, provider) in providers {
        if id.trim().is_empty()
            || provider.client_id.trim().is_empty()
            || matches!(provider.provider_type, ProviderType::Oidc)
                && provider
                    .issuer
                    .as_ref()
                    .is_none_or(|issuer| issuer.trim().is_empty())
        {
            return Err(BootstrapError::InvalidOAuthInput);
        }
        result.insert(
            id,
            OAuthProviderConfig {
                provider_type: match provider.provider_type {
                    ProviderType::Github => "github",
                    ProviderType::Oidc => "oidc",
                }
                .into(),
                issuer: provider.issuer,
                client_id: Some(provider.client_id),
                client_secret: provider.client_secret,
                client_secret_file: None,
                display_name: provider.display_name,
                scopes: provider.scopes,
                role_claims: provider.role_claims,
            },
        );
    }
    Ok(result)
}
