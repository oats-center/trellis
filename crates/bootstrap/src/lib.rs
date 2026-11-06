//! NATS and TLS material for Trellis installations: an operator and its
//! system account, the accounts Trellis owns on a NATS operator's servers,
//! NATS server configuration, and generated certificates.
mod certificate;
mod error;
mod files;
mod nats_config;
mod nats_material;
mod standalone;
mod tls;
mod types;

pub use certificate::{inspect_certificate, CertificateError, CertificateSummary};
pub use error::{BootstrapError, NatsConfigError};
pub use files::write_private_file;
pub use nats_config::{
    nats_accounts, read_nats_listen_ports, render_local_nats_config, render_nats_config,
    slug_from_name, NatsTlsFiles, NatsWebsocket,
};
pub use nats_material::{
    render_nats_operator, NatsOperator, NatsUserKeys, TrellisAccounts, AUTH_ACCOUNT,
    SYSTEM_ACCOUNT, TRELLIS_ACCOUNT,
};
pub use standalone::{generate_nats_bootstrap, NatsBootstrapOptions};
pub use tls::{ensure_generated_tls, issue_certificates, GeneratedTls, IssuedCertificates};
pub use types::NatsListeners;

#[cfg(test)]
mod tests;
