use std::path::PathBuf;

use crate::{
    DEFAULT_AUTH_ACCOUNT, DEFAULT_NATS_SERVER_URL, DEFAULT_NATS_WEBSOCKET_URL,
    DEFAULT_OPERATOR_NAME, DEFAULT_PUBLIC_ORIGIN, DEFAULT_SYSTEM_ACCOUNT, DEFAULT_TRELLIS_ACCOUNT,
    DEFAULT_TRELLIS_NAME, DEFAULT_TRELLIS_PORT,
};

/// Shared NATS bootstrap names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsBootstrapNames {
    /// NATS operator name.
    pub operator_name: String,
    /// System account name.
    pub system_account: String,
    /// Auth account name.
    pub auth_account: String,
    /// Trellis service account name.
    pub trellis_account: String,
    /// Optional NATS server name override written to `nats.conf`.
    pub server_name: Option<String>,
}

impl Default for NatsBootstrapNames {
    fn default() -> Self {
        Self {
            operator_name: DEFAULT_OPERATOR_NAME.to_string(),
            system_account: DEFAULT_SYSTEM_ACCOUNT.to_string(),
            auth_account: DEFAULT_AUTH_ACCOUNT.to_string(),
            trellis_account: DEFAULT_TRELLIS_ACCOUNT.to_string(),
            server_name: None,
        }
    }
}

/// Shared NATS bootstrap configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NatsBootstrapConfig {
    /// NATS bootstrap names.
    pub names: NatsBootstrapNames,
}

/// Options for generating a NATS bootstrap directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsBootstrapOptions {
    /// Output directory for generated NATS config, credentials, and key material files.
    pub out: PathBuf,
    /// Replace an existing non-empty output directory.
    pub force: bool,
    /// NATS bootstrap configuration.
    pub config: NatsBootstrapConfig,
}

impl NatsBootstrapOptions {
    /// Build options using bootstrap defaults.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> Self {
        Self {
            out: out.into(),
            force: false,
            config: NatsBootstrapConfig::default(),
        }
    }
}

/// Runtime-facing Trellis bootstrap configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrellisRuntimeBootstrapConfig {
    /// Human-readable Trellis name written to runtime config.
    pub name: String,
    /// Trellis HTTP port written to `config.toml`.
    pub trellis_port: u16,
    /// Native NATS URL used by server-side Trellis services.
    pub nats_server_url: String,
    /// Browser-facing NATS websocket URL advertised to clients.
    pub nats_websocket_url: String,
    /// Public HTTP origin for OAuth redirects.
    pub public_origin: String,
}

impl Default for TrellisRuntimeBootstrapConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_TRELLIS_NAME.to_string(),
            trellis_port: DEFAULT_TRELLIS_PORT,
            nats_server_url: DEFAULT_NATS_SERVER_URL.to_string(),
            nats_websocket_url: DEFAULT_NATS_WEBSOCKET_URL.to_string(),
            public_origin: DEFAULT_PUBLIC_ORIGIN.to_string(),
        }
    }
}

/// Options for generating a complete Trellis bootstrap bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrellisBootstrapOptions {
    /// Output directory for generated NATS and Trellis bootstrap files.
    pub out: PathBuf,
    /// Replace an existing non-empty output directory.
    pub force: bool,
    /// NATS bootstrap configuration.
    pub nats: NatsBootstrapConfig,
    /// Runtime-facing Trellis bootstrap configuration.
    pub runtime: TrellisRuntimeBootstrapConfig,
}

impl TrellisBootstrapOptions {
    /// Build options using bootstrap defaults.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> Self {
        Self {
            out: out.into(),
            force: false,
            nats: NatsBootstrapConfig::default(),
            runtime: TrellisRuntimeBootstrapConfig::default(),
        }
    }
}

/// Non-secret identity metadata emitted with generated NATS bootstrap material.
///
/// This metadata records account and user public keys plus the account names
/// needed by runtime config and auth-callout environment rendering. It excludes
/// user seeds, signing seeds, and other secret material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedMetadata {
    /// Name of the generated NATS system account.
    pub(crate) system_account_name: String,
    /// Public key for the generated NATS system account.
    pub(crate) system_account_public_key: String,
    /// Public key for the generated system user.
    pub(crate) system_user_public_key: String,
    /// Name of the generated auth account.
    pub(crate) auth_account_name: String,
    /// Public key for the generated auth account.
    pub(crate) auth_account_public_key: String,
    /// Name of the generated Trellis service account.
    pub(crate) trellis_account_name: String,
    /// Public key for the generated Trellis service account.
    pub(crate) trellis_account_public_key: String,
    /// Public key for the auth service user.
    pub(crate) auth_user_public_key: String,
    /// Public key for the Trellis service user.
    pub(crate) trellis_user_public_key: String,
}
