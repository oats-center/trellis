//! Trellis bootstrap generation for NATS and runtime configuration.
//!
//! This crate produces bootstrap material for `trellis-server init`: NATS server config,
//! generated NATS credentials, and Trellis runtime TOML config.
mod constants;
mod error;
mod generation;
mod nats_config;
mod nats_material;
mod output;
mod runtime_config;
mod tls;
mod types;
mod validate;

pub use constants::{
    DEFAULT_AUTH_ACCOUNT, DEFAULT_CONSOLE_ORIGIN, DEFAULT_CONSOLE_PORT, DEFAULT_NATS_MONITOR_PORT,
    DEFAULT_NATS_PORT, DEFAULT_NATS_SERVER_URL, DEFAULT_NATS_WEBSOCKET_PORT,
    DEFAULT_NATS_WEBSOCKET_URL, DEFAULT_OPERATOR_NAME, DEFAULT_PUBLIC_ORIGIN,
    DEFAULT_SYSTEM_ACCOUNT, DEFAULT_TRELLIS_ACCOUNT, DEFAULT_TRELLIS_NAME, DEFAULT_TRELLIS_PORT,
};
pub use error::{BootstrapError, NatsConfigError};
pub use generation::{
    bundle_ca_directory, generate_nats_bootstrap, generate_trellis_bootstrap, BUNDLE_CA_DIRECTORY,
};
pub use nats_config::{
    read_nats_listen_ports, render_auth_callout_env, render_local_jwt_config,
    render_local_nats_config, render_nats_config, slug_from_name, NatsTlsFiles,
};
pub use runtime_config::{render_trellis_config, trellis_runtime_config};
pub use tls::{
    issue_certificates, renew_generated_certificates, GeneratedTlsSummary, IssuedCertificates,
    RenewedCertificate,
};
pub use types::{
    BootstrapAuthorizationPolicy, BootstrapTls, GeneratedMetadata, NatsBootstrapConfig,
    NatsBootstrapNames, NatsBootstrapOptions, NatsListeners, SuppliedTls, TrellisBootstrapOptions,
    TrellisRuntimeBootstrapConfig,
};
pub use validate::validate_output_dir;

#[cfg(test)]
mod tests;
