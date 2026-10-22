#[cfg(feature = "runtime")]
use std::path::PathBuf;

#[cfg(feature = "runtime")]
use clap::{Args, Subcommand, ValueEnum};

#[cfg(feature = "runtime")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
/// Certificate source for generated listeners.
pub enum InitTlsMode {
    /// Generate an installation CA and short-lived certificates (default).
    Generated,
    /// Use operator-supplied PEM certificates and keys.
    Supplied,
    /// Plaintext restricted to loopback origins and listeners.
    LoopbackDevelopment,
}

#[cfg(feature = "runtime")]
#[derive(Debug, Args)]
/// Generate Trellis runtime config and NATS bootstrap material.
pub struct InitConfigArgs {
    #[arg(long)]
    /// Output directory for generated Trellis bootstrap files.
    pub out: PathBuf,

    #[arg(long)]
    /// Replace an existing non-empty output directory.
    pub force: bool,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_TRELLIS_NAME.to_string())]
    /// Human-readable Trellis name used in generated config.
    pub name: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_OPERATOR_NAME.to_string())]
    /// NATS operator name.
    pub operator_name: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_SYSTEM_ACCOUNT.to_string())]
    /// NATS system account name.
    pub system_account: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_AUTH_ACCOUNT.to_string())]
    /// Trellis auth account name.
    pub auth_account: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_TRELLIS_ACCOUNT.to_string())]
    /// Trellis runtime account name.
    pub trellis_account: String,

    #[arg(long)]
    /// Override the NATS server name written to nats.conf.
    pub server_name: Option<String>,

    #[arg(long, default_value_t = 3000)]
    /// Trellis HTTP port written to config.toml.
    pub trellis_port: u16,

    #[arg(long, default_value_t = 4222)]
    /// Native NATS listen port written to nats.conf.
    pub nats_port: u16,

    #[arg(long, default_value_t = 8222)]
    /// NATS HTTP monitoring port written to nats.conf.
    pub nats_monitor_port: u16,

    #[arg(long, default_value_t = 8080)]
    /// Browser websocket listen port written to nats.conf.
    pub nats_ws_port: u16,

    #[arg(long)]
    /// Absolute writable data directory for an independently managed NATS server.
    pub nats_data_dir: Option<PathBuf>,

    #[arg(long)]
    /// Native NATS server URL for Trellis services; defaults to tls://localhost:<nats-port>
    /// (nats:// in loopback development).
    pub nats_server_url: Option<String>,

    #[arg(long)]
    /// Browser-facing NATS websocket URL for Trellis clients; defaults to
    /// wss://localhost:<nats-ws-port> (ws:// in loopback development).
    pub nats_websocket_url: Option<String>,

    #[arg(long)]
    /// Public Console origin; defaults to https://localhost:<trellis-port>
    /// (http:// in loopback development).
    pub public_origin: Option<String>,

    #[arg(long)]
    /// Public OAuth origin, distinct from the Console origin; defaults to
    /// https://localhost:<oauth-port> (http:// in loopback development).
    pub oauth_origin: Option<String>,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_OAUTH_PORT)]
    /// Dedicated OAuth listener port.
    pub oauth_port: u16,

    #[arg(long, value_enum, default_value_t = InitTlsMode::Generated)]
    /// Certificate source. Generated TLS creates an installation CA that
    /// browsers and native clients must trust explicitly.
    pub tls: InitTlsMode,

    #[arg(long)]
    /// Directory receiving the generated CA private key; defaults to
    /// `<out>-tls-ca`. Keep it out of every runtime and broker mount.
    pub tls_ca_dir: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM certificate chain naming the Console and OAuth hosts.
    pub tls_certificate_chain: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM private key for --tls-certificate-chain.
    pub tls_private_key: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM certificate chain naming the NATS and WebSocket hosts.
    pub nats_tls_certificate_chain: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM private key for --nats-tls-certificate-chain.
    pub nats_tls_private_key: Option<PathBuf>,

    #[arg(long)]
    /// Private CA that issued the supplied NATS certificate, trusted by the runtime.
    pub tls_ca_certificate: Option<PathBuf>,

    #[arg(long = "trusted-proxy", value_name = "CIDR")]
    /// Serve Console and OAuth as plaintext behind an HTTPS-terminating
    /// reverse proxy at this network; repeatable. Public origins stay HTTPS.
    pub trusted_proxy: Vec<ipnet::IpNet>,

    #[arg(long)]
    /// Serve NATS WebSockets as plaintext behind an HTTPS-terminating reverse
    /// proxy. Native NATS always terminates its own TLS.
    pub nats_websocket_behind_proxy: bool,

    #[arg(long = "extra-origin", value_name = "ORIGIN")]
    /// Additional allowed browser origin, for callers served elsewhere; repeatable.
    pub extra_origin: Vec<String>,
    /// HTTP listener IP address.
    #[arg(long)]
    pub bind_address: Option<std::net::IpAddr>,
    /// Authorization context lifetime in seconds.
    #[arg(long)]
    pub auth_context_lifetime_seconds: Option<u64>,
    /// Authorization refresh lead in seconds.
    #[arg(long)]
    pub auth_refresh_lead_seconds: Option<u64>,
    /// Earlier-only authorization refresh jitter in seconds.
    #[arg(long)]
    pub auth_refresh_jitter_seconds: Option<u64>,
    /// Minimum authorization context lifetime in seconds.
    #[arg(long)]
    pub auth_minimum_context_lifetime_seconds: Option<u64>,
    /// Maximum signed OAuth admission lifetime in seconds.
    #[arg(long)]
    pub auth_oauth_admission_lifetime_seconds: Option<u64>,
    /// Authorization clock-skew allowance in seconds.
    #[arg(long)]
    pub auth_allowed_clock_skew_seconds: Option<u64>,
    /// Private JSON provider input file; repeatable, duplicate IDs are rejected.
    #[arg(long)]
    pub oauth_providers_file: Vec<PathBuf>,
}

#[cfg(feature = "runtime")]
#[derive(Debug, Args)]
/// Manage Trellis-generated TLS certificates.
pub struct TlsCommand {
    #[command(subcommand)]
    pub command: TlsSubcommand,
}

#[cfg(feature = "runtime")]
#[derive(Debug, Subcommand)]
/// Generated TLS operations.
pub enum TlsSubcommand {
    /// Reissue the bundle's leaf certificates from its installation CA.
    Renew(TlsRenewArgs),
    /// Show the installation CA fingerprint and certificate expiry.
    Status(TlsStatusArgs),
}

#[cfg(feature = "runtime")]
#[derive(Debug, Args)]
/// Renew generated leaf certificates without changing client trust.
pub struct TlsRenewArgs {
    /// Bundle directory written by `trellis init config --out`.
    #[arg(long)]
    pub bundle: PathBuf,
    /// CA directory holding the private key; defaults to `<bundle>-tls-ca`.
    #[arg(long)]
    pub ca_dir: Option<PathBuf>,
}

#[cfg(feature = "runtime")]
#[derive(Debug, Args)]
/// Report generated TLS identity for a runtime configuration.
pub struct TlsStatusArgs {
    /// Runtime configuration file.
    #[arg(long)]
    pub config: PathBuf,
}

#[cfg(feature = "runtime")]
#[derive(Debug, Args)]
/// Run one-time initialization workflows.
pub struct InitCommand {
    #[command(subcommand)]
    pub command: InitSubcommand,
}

#[cfg(feature = "runtime")]
#[derive(Debug, Subcommand)]
/// Initialization operations.
pub enum InitSubcommand {
    /// Generate Trellis runtime config and NATS bootstrap material.
    Config(Box<InitConfigArgs>),
    /// Initialize the protected administrator with a password or setup link.
    Admin(InitAdminArgs),
}

#[cfg(feature = "runtime")]
#[derive(Debug, Args)]
/// Initialize the protected administrator in the configured runtime storage.
pub struct InitAdminArgs {
    /// Use the Linux system/FHS host profile instead of the user profile.
    #[arg(long)]
    pub system: bool,
    /// Runtime configuration identifying the installation and custody files.
    #[arg(long)]
    pub config: PathBuf,
    /// Starting administrator username.
    #[arg(long, default_value = "admin")]
    pub username: String,
    /// Starting password; omit for a one-time password-setup link.
    #[arg(long, conflicts_with = "password_stdin")]
    pub password: Option<String>,
    /// Read the starting password from stdin instead of argv.
    #[arg(long)]
    pub password_stdin: bool,
}
