use std::net::IpAddr;
use std::path::PathBuf;

use crate::{
    DEFAULT_AUTH_ACCOUNT, DEFAULT_CONSOLE_ORIGIN, DEFAULT_CONSOLE_PORT, DEFAULT_NATS_MONITOR_PORT,
    DEFAULT_NATS_PORT, DEFAULT_NATS_SERVER_URL, DEFAULT_NATS_WEBSOCKET_PORT,
    DEFAULT_NATS_WEBSOCKET_URL, DEFAULT_OPERATOR_NAME, DEFAULT_PUBLIC_ORIGIN,
    DEFAULT_SYSTEM_ACCOUNT, DEFAULT_TRELLIS_ACCOUNT, DEFAULT_TRELLIS_NAME, DEFAULT_TRELLIS_PORT,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsBootstrapConfig {
    /// NATS bootstrap names.
    pub names: NatsBootstrapNames,
    /// Native NATS listen port written to `nats.conf`.
    pub nats_port: u16,
    /// NATS HTTP monitoring port written to `nats.conf`.
    pub monitor_port: u16,
    /// Browser websocket listen port written to `nats.conf`.
    pub websocket_port: u16,
}

impl Default for NatsBootstrapConfig {
    fn default() -> Self {
        Self {
            names: NatsBootstrapNames::default(),
            nats_port: DEFAULT_NATS_PORT,
            monitor_port: DEFAULT_NATS_MONITOR_PORT,
            websocket_port: DEFAULT_NATS_WEBSOCKET_PORT,
        }
    }
}

/// NATS listener ports resolved from a generated managed bundle's `nats.conf`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NatsListeners {
    /// Native NATS client listener port.
    pub native: u16,
    /// HTTP monitoring listener port.
    pub monitor: u16,
    /// Browser websocket listener port.
    pub websocket: u16,
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
    /// Installation ULID generated once for this bootstrap bundle.
    pub instance_id: String,
    /// The Console's public origin, distinct from the Trellis origin.
    pub console_origin: String,
    /// The Console's listener port.
    pub console_port: u16,
    /// Certificate source for every TLS listener in the bundle.
    pub tls: BootstrapTls,
    /// Reverse-proxy networks; non-empty serves Trellis and the Console as plaintext
    /// behind an HTTPS-terminating proxy at those addresses.
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// Serve browser WebSockets as plaintext behind an HTTPS-terminating proxy.
    /// Native NATS always terminates its own TLS.
    pub nats_websocket_behind_proxy: bool,
    /// The Trellis listener relays every WebSocket upgrade to the broker, so
    /// clients reach NATS at the Trellis origin with its certificate. The
    /// broker's WebSocket listener then serves plaintext for the runtime, and
    /// `nats_websocket_url` is not used.
    pub nats_websocket_via_runtime: bool,
    /// Advertise only the WebSocket to clients; `nats_server_url` is then only
    /// where the runtime itself reaches NATS.
    pub nats_websocket_only: bool,
    /// Human-readable Trellis name written to runtime config.
    pub name: String,
    /// Trellis listener port written to `config.toml`.
    pub trellis_port: u16,
    /// Native NATS URL used by server-side Trellis services.
    pub nats_server_url: String,
    /// Browser-facing NATS websocket URL advertised to clients.
    pub nats_websocket_url: String,
    /// The Trellis origin: sign-in's issuer and the address clients use.
    pub public_origin: String,
    /// Optional HTTP listener address; omission retains the production default.
    pub bind_address: Option<IpAddr>,
    /// Authorization timing without access to generated signing material.
    pub authorization: BootstrapAuthorizationPolicy,
    /// Write `[paths]` that keep data, state, cache, runtime files and logs in
    /// folders inside the bundle. Off leaves them to the server's host profile,
    /// as for a read-only bundle served with `--system`.
    pub bundle_paths: bool,
    /// `trellis-server` runs the generated NATS server itself. Off, it
    /// connects to a NATS server the operator runs from the generated files.
    pub managed_nats: bool,
}

impl Default for TrellisRuntimeBootstrapConfig {
    fn default() -> Self {
        Self {
            instance_id: ulid::Ulid::new().to_string(),
            console_origin: DEFAULT_CONSOLE_ORIGIN.to_string(),
            console_port: DEFAULT_CONSOLE_PORT,
            tls: BootstrapTls::Generated { ca_directory: None },
            trusted_proxies: Vec::new(),
            nats_websocket_behind_proxy: false,
            nats_websocket_via_runtime: false,
            nats_websocket_only: false,
            name: DEFAULT_TRELLIS_NAME.to_string(),
            trellis_port: DEFAULT_TRELLIS_PORT,
            nats_server_url: DEFAULT_NATS_SERVER_URL.to_string(),
            nats_websocket_url: DEFAULT_NATS_WEBSOCKET_URL.to_string(),
            public_origin: DEFAULT_PUBLIC_ORIGIN.to_string(),
            bind_address: None,
            authorization: BootstrapAuthorizationPolicy::default(),
            bundle_paths: true,
            managed_nats: true,
        }
    }
}

/// Certificate source for a bundle's TLS listeners.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BootstrapTls {
    /// Generate an installation CA and short-lived leaf certificates.
    ///
    /// `ca_directory` receives the CA private key and must stay outside every
    /// role's mount; `None` selects `<out>-tls-ca` beside the bundle.
    Generated {
        /// Directory for the CA private key.
        ca_directory: Option<PathBuf>,
    },
    /// Copy operator-supplied PEM files into the bundle.
    Supplied(SuppliedTls),
    /// Plaintext restricted to loopback origins and listeners.
    LoopbackDevelopment,
}

/// Operator-supplied PEM certificate files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuppliedTls {
    /// Certificate chain naming the Console and OAuth hosts; omitted behind
    /// a trusted proxy, which serves them.
    pub certificate_chain: Option<PathBuf>,
    /// Private key for `certificate_chain`.
    pub private_key: Option<PathBuf>,
    /// Certificate chain naming the native NATS and WebSocket hosts.
    pub nats_certificate_chain: PathBuf,
    /// Private key for `nats_certificate_chain`.
    pub nats_private_key: PathBuf,
    /// Private CA the runtime trusts for NATS; omit for publicly trusted certificates.
    pub ca_certificate: Option<PathBuf>,
}

/// Bootstrap-owned authorization timing policy; secret paths remain generated.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootstrapAuthorizationPolicy {
    /// Signed context lifetime in seconds.
    pub context_lifetime_seconds: Option<u64>,
    /// Signed OAuth admission lifetime in seconds (production default: 300).
    pub oauth_admission_lifetime_seconds: Option<u64>,
    /// Authorization clock-skew allowance in seconds (production default: 30).
    pub allowed_clock_skew_seconds: Option<u64>,
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
    /// Public key of the server-substituted auth-callout sentinel.
    pub(crate) auth_sentinel_public_key: String,
    /// Public key for the Trellis service user.
    pub(crate) trellis_user_public_key: String,
}
