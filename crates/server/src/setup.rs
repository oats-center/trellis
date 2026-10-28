//! One-time installation setup on the host that runs the server: generating
//! its configuration bundle and TLS, seeding the first administrator, and
//! renewing generated certificates. Talking to a running Trellis is `trellis`.

use std::path::PathBuf;

use miette::{miette, IntoDiagnostic as _};
use serde_json::json;
use trellis_bootstrap::{
    generate_trellis_bootstrap, BootstrapError, BootstrapTls, SuppliedTls, TrellisBootstrapOptions,
};

/// How setup commands report their result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OutputFormat {
    /// Readable lines for people.
    #[default]
    Text,
    /// One JSON document for scripts.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
/// How browsers reach the broker's NATS WebSocket.
pub(crate) enum InitWebsocketMode {
    /// On the Console origin at `/nats`, relayed by the runtime, so browsers
    /// trust one host and certificate. Default with generated TLS.
    Relay,
    /// At the broker's own WebSocket listener and certificate.
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
/// Certificate source for generated listeners.
pub(crate) enum InitTlsMode {
    /// Generate an installation CA and short-lived certificates. Browsers
    /// and clients accept them by the CA's fingerprint, so they suit labs and
    /// tests; `--dev` uses them.
    Generated,
    /// Use operator-supplied PEM certificates and keys.
    Supplied,
    /// Plaintext restricted to loopback origins and listeners.
    LoopbackDevelopment,
}

#[derive(Debug, clap::Args)]
/// Generate Trellis runtime config and NATS bootstrap material.
pub(crate) struct InitConfigArgs {
    #[arg(long)]
    /// Output directory for a self-contained installation that also keeps its
    /// data and logs. Omitted, the installation goes where the server looks by
    /// default (~/.config/trellis, or /etc/trellis with --system) and its data
    /// goes to the profile's data, state, cache and log folders.
    pub(crate) out: Option<PathBuf>,

    #[arg(long)]
    /// Connect to a NATS server you run from the generated files instead of
    /// having trellis-server start one.
    pub(crate) external_nats: bool,

    #[arg(long, conflicts_with_all = ["oauth_origin", "oauth_port"])]
    /// Development mode: the Console's port and its one certificate also carry
    /// sign-in (under /auth) and browsers' NATS, so a browser on another
    /// machine accepts a single certificate. Not for production.
    pub(crate) dev: bool,

    #[arg(long, default_value = "localhost")]
    /// Hostname the default Console, sign-in and NATS addresses use.
    pub(crate) host: String,

    #[arg(long)]
    /// Replace an existing non-empty output directory.
    pub(crate) force: bool,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_TRELLIS_NAME.to_string())]
    /// Human-readable Trellis name used in generated config.
    pub(crate) name: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_OPERATOR_NAME.to_string())]
    /// NATS operator name.
    pub(crate) operator_name: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_SYSTEM_ACCOUNT.to_string())]
    /// NATS system account name.
    pub(crate) system_account: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_AUTH_ACCOUNT.to_string())]
    /// Trellis auth account name.
    pub(crate) auth_account: String,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_TRELLIS_ACCOUNT.to_string())]
    /// Trellis runtime account name.
    pub(crate) trellis_account: String,

    #[arg(long)]
    /// Override the NATS server name written to nats.conf.
    pub(crate) server_name: Option<String>,

    #[arg(long, default_value_t = 3000)]
    /// Trellis HTTP port written to config.toml.
    pub(crate) trellis_port: u16,

    #[arg(long, default_value_t = 4222)]
    /// Native NATS listen port written to nats.conf.
    pub(crate) nats_port: u16,

    #[arg(long, default_value_t = 8222)]
    /// NATS HTTP monitoring port written to nats.conf.
    pub(crate) nats_monitor_port: u16,

    #[arg(long, default_value_t = 8080)]
    /// Browser websocket listen port written to nats.conf.
    pub(crate) nats_ws_port: u16,

    #[arg(long)]
    /// Absolute writable data directory for an independently managed NATS server.
    pub(crate) nats_data_dir: Option<PathBuf>,

    #[arg(long)]
    /// Native NATS server URL for Trellis services; defaults to tls://localhost:<nats-port>
    /// (nats:// in loopback development).
    pub(crate) nats_server_url: Option<String>,

    #[arg(long)]
    /// Browser-facing NATS websocket URL for Trellis clients; defaults to
    /// wss://localhost:<nats-ws-port> (ws:// in loopback development).
    pub(crate) nats_websocket_url: Option<String>,

    #[arg(long)]
    /// Public Console origin; defaults to https://localhost:<trellis-port>
    /// (http:// in loopback development).
    pub(crate) public_origin: Option<String>,

    #[arg(long)]
    /// Public OAuth origin, distinct from the Console origin; defaults to
    /// https://localhost:<oauth-port> (http:// in loopback development).
    pub(crate) oauth_origin: Option<String>,

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_OAUTH_PORT)]
    /// Dedicated OAuth listener port.
    pub(crate) oauth_port: u16,

    #[arg(long, value_enum)]
    /// Certificate source. Required without --dev, which generates them.
    pub(crate) tls: Option<InitTlsMode>,

    #[arg(long)]
    /// Directory receiving the generated CA private key; defaults to the
    /// installation's `tls-ca` folder, or `<out>-tls-ca` with `--system`.
    /// Keep it out of every runtime and broker mount.
    pub(crate) tls_ca_dir: Option<PathBuf>,

    #[arg(long, requires = "tls_private_key")]
    /// Supplied PEM certificate chain naming the Console and OAuth hosts;
    /// not needed behind a --trusted-proxy.
    pub(crate) tls_certificate_chain: Option<PathBuf>,

    #[arg(long, requires = "tls_certificate_chain")]
    /// Supplied PEM private key for --tls-certificate-chain.
    pub(crate) tls_private_key: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM certificate chain naming the NATS and WebSocket hosts.
    pub(crate) nats_tls_certificate_chain: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM private key for --nats-tls-certificate-chain.
    pub(crate) nats_tls_private_key: Option<PathBuf>,

    #[arg(long)]
    /// Private CA that issued the supplied NATS certificate, trusted by the runtime.
    pub(crate) tls_ca_certificate: Option<PathBuf>,

    #[arg(long = "trusted-proxy", value_name = "CIDR")]
    /// Serve Console and OAuth as plaintext behind an HTTPS-terminating
    /// reverse proxy at this network; repeatable. Public origins stay HTTPS.
    pub(crate) trusted_proxy: Vec<ipnet::IpNet>,

    #[arg(long)]
    /// Serve NATS WebSockets as plaintext behind an HTTPS-terminating reverse
    /// proxy. Native NATS always terminates its own TLS.
    pub(crate) nats_websocket_behind_proxy: bool,

    #[arg(long, value_enum)]
    /// How browsers reach NATS WebSockets. `relay` serves them on the Console
    /// origin at `/nats` through the runtime, so one certificate covers both;
    /// the broker's WebSocket port then serves plaintext and must stay
    /// private. Defaults to `relay` with generated TLS unless
    /// --nats-websocket-url or --nats-websocket-behind-proxy is given, and to
    /// `direct` otherwise.
    pub(crate) nats_websocket: Option<InitWebsocketMode>,

    #[arg(long = "extra-origin", value_name = "ORIGIN")]
    /// Additional allowed browser origin, for callers served elsewhere; repeatable.
    pub(crate) extra_origin: Vec<String>,
    /// Address Console and sign-in listen on: an IP or a hostname that
    /// resolves to one of this machine's addresses.
    #[arg(long, value_name = "HOST", value_parser = parse_bind_address)]
    pub(crate) bind_address: Option<std::net::IpAddr>,
    /// Authorization context lifetime in seconds.
    #[arg(long)]
    pub(crate) auth_context_lifetime_seconds: Option<u64>,
    /// Maximum signed OAuth admission lifetime in seconds.
    #[arg(long)]
    pub(crate) auth_oauth_admission_lifetime_seconds: Option<u64>,
    /// Authorization clock-skew allowance in seconds.
    #[arg(long)]
    pub(crate) auth_allowed_clock_skew_seconds: Option<u64>,
}

#[derive(Debug, clap::Subcommand)]
/// Generated TLS operations.
pub(crate) enum TlsSubcommand {
    /// Reissue the bundle's leaf certificates from its installation CA.
    Renew(TlsRenewArgs),
    /// Show the installation CA fingerprint and certificate expiry.
    Status(TlsStatusArgs),
    /// Issue a certificate for some hosts from a new single-use CA, to pass
    /// to `init --tls supplied` on a lab or test installation.
    Issue(TlsIssueArgs),
}

#[derive(Debug, clap::Args)]
/// Issue a server certificate from a new CA that is discarded afterwards.
pub(crate) struct TlsIssueArgs {
    /// Folder that receives ca.crt, server.crt and server.key.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// Hostname or IP address the certificate names; repeatable.
    #[arg(long = "host", value_name = "HOST", required = true)]
    pub(crate) hosts: Vec<String>,
    /// Name of the issuing CA, shown when someone inspects the certificate.
    #[arg(long, default_value = "Trellis test CA")]
    pub(crate) name: String,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, clap::Args)]
/// Renew generated leaf certificates without changing client trust.
pub(crate) struct TlsRenewArgs {
    /// Bundle directory written by `trellis-server init --out`.
    #[arg(long)]
    pub(crate) bundle: PathBuf,
    /// CA directory holding the private key; defaults to the bundle's
    /// `tls-ca` folder, else `<bundle>-tls-ca`.
    #[arg(long)]
    pub(crate) ca_dir: Option<PathBuf>,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, clap::Args)]
/// Report an installation's generated certificates.
pub(crate) struct TlsStatusArgs {
    #[command(flatten)]
    pub(crate) profile: crate::ProfileArgs,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

pub(crate) fn init_config(
    format: OutputFormat,
    args: &InitConfigArgs,
    system: bool,
    default_out: PathBuf,
) -> miette::Result<()> {
    let out = args.out.clone().unwrap_or(default_out);
    let mut options = TrellisBootstrapOptions::new(out.clone());
    options.force = args.force;
    options.runtime.name = args.name.clone();
    options.runtime.trellis_port = args.trellis_port;
    let tls = match (args.tls, args.dev) {
        (Some(tls), _) => tls,
        (None, true) => InitTlsMode::Generated,
        (None, false) => {
            return Err(miette!(
                "choose where this installation's certificates come from: --tls supplied \
                with certificates browsers already trust, --tls generated for a lab or test \
                installation, or --dev for development"
            ))
        }
    };
    let loopback = tls == InitTlsMode::LoopbackDevelopment;
    if args.dev
        && (loopback
            || args.nats_websocket == Some(InitWebsocketMode::Direct)
            || args.nats_websocket_url.is_some())
    {
        return Err(miette!(
            "--dev serves everything over TLS on the Console's port; it can't be combined \
            with --tls loopback-development, --nats-websocket direct or --nats-websocket-url"
        ));
    }
    let host = &args.host;
    let (http, native, websocket) = if loopback {
        ("http", "nats", "ws")
    } else {
        ("https", "tls", "wss")
    };
    options.runtime.nats_server_url = args
        .nats_server_url
        .clone()
        .unwrap_or_else(|| format!("{native}://{host}:{}", args.nats_port));
    options.runtime.nats_websocket_url = args
        .nats_websocket_url
        .clone()
        .unwrap_or_else(|| format!("{websocket}://{host}:{}", args.nats_ws_port));
    options.runtime.public_origin = args
        .public_origin
        .clone()
        .unwrap_or_else(|| format!("{http}://{host}:{}", args.trellis_port));
    let relay = match args.nats_websocket {
        Some(InitWebsocketMode::Relay) => {
            if args.nats_websocket_url.is_some() || args.nats_websocket_behind_proxy {
                return Err(miette!(
                    "--nats-websocket relay cannot be combined with --nats-websocket-url or \
                    --nats-websocket-behind-proxy"
                ));
            }
            true
        }
        Some(InitWebsocketMode::Direct) => false,
        // Out of the box, a browser trusting the generated CA for the Console
        // also reaches NATS; a separate WebSocket port would need its own trust.
        None => {
            args.dev
                || (tls == InitTlsMode::Generated
                    && args.nats_websocket_url.is_none()
                    && !args.nats_websocket_behind_proxy)
        }
    };
    if relay {
        // Reported for operators; the runtime derives the same URL itself.
        options.runtime.nats_websocket_url = format!(
            "{}{}",
            options
                .runtime
                .public_origin
                .trim_end_matches('/')
                .replacen("http", "ws", 1),
            trellis_runtime::nats_websocket::PATH
        );
    }
    if args.dev {
        options.runtime.oauth_origin = format!(
            "{}{}",
            options.runtime.public_origin.trim_end_matches('/'),
            trellis_runtime::config::DEVELOPMENT_SIGN_IN_PATH
        );
        options.runtime.oauth_port = args.trellis_port;
    } else {
        options.runtime.oauth_origin = args
            .oauth_origin
            .clone()
            .unwrap_or_else(|| format!("{http}://{host}:{}", args.oauth_port));
        options.runtime.oauth_port = args.oauth_port;
    }
    options.runtime.development = args.dev;
    options.runtime.tls = match tls {
        // A system installation is the one roles mount, so its CA stays beside it.
        InitTlsMode::Generated => BootstrapTls::Generated {
            ca_directory: args
                .tls_ca_dir
                .clone()
                .or_else(|| (!system).then(|| out.join(trellis_bootstrap::BUNDLE_CA_DIRECTORY))),
        },
        InitTlsMode::Supplied => BootstrapTls::Supplied(SuppliedTls {
            certificate_chain: args.tls_certificate_chain.clone(),
            private_key: args.tls_private_key.clone(),
            nats_certificate_chain: required_path(
                &args.nats_tls_certificate_chain,
                "--nats-tls-certificate-chain",
            )?,
            nats_private_key: required_path(&args.nats_tls_private_key, "--nats-tls-private-key")?,
            ca_certificate: args.tls_ca_certificate.clone(),
        }),
        InitTlsMode::LoopbackDevelopment => BootstrapTls::LoopbackDevelopment,
    };
    // A `--system` server keeps its mutable files in FHS locations, and the
    // default installation uses the profile's folders like the server does.
    options.runtime.bundle_paths = !system && args.out.is_some();
    options.runtime.managed_nats = !args.external_nats;
    options.runtime.trusted_proxies = args.trusted_proxy.clone();
    options.runtime.nats_websocket_behind_proxy = args.nats_websocket_behind_proxy;
    options.runtime.nats_websocket_via_runtime = relay;
    options.runtime.extra_origins = args.extra_origin.clone();
    options.runtime.bind_address = args.bind_address;
    options.runtime.authorization = trellis_bootstrap::BootstrapAuthorizationPolicy {
        context_lifetime_seconds: args.auth_context_lifetime_seconds,
        oauth_admission_lifetime_seconds: args.auth_oauth_admission_lifetime_seconds,
        allowed_clock_skew_seconds: args.auth_allowed_clock_skew_seconds,
    };
    options.nats.nats_port = args.nats_port;
    options.nats.monitor_port = args.nats_monitor_port;
    options.nats.websocket_port = args.nats_ws_port;
    options.nats.names.operator_name = args.operator_name.clone();
    options.nats.names.system_account = args.system_account.clone();
    options.nats.names.auth_account = args.auth_account.clone();
    options.nats.names.trellis_account = args.trellis_account.clone();
    options.nats.names.server_name = args.server_name.clone();

    if args
        .nats_data_dir
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(miette!("--nats-data-dir must be absolute"));
    }
    let generated = generate_trellis_bootstrap(&options).map_err(bootstrap_report)?;
    if let Some(data) = &args.nats_data_dir {
        let nats = out.join("nats");
        std::fs::create_dir_all(data.join("jwt")).into_diagnostic()?;
        let jwt = std::fs::read_to_string(nats.join("jwt.conf")).into_diagnostic()?;
        std::fs::write(
            nats.join("jwt.conf"),
            trellis_bootstrap::render_local_jwt_config(
                &jwt,
                &data.join("jwt").display().to_string(),
            ),
        )
        .into_diagnostic()?;
        std::fs::write(
            nats.join("nats.conf"),
            trellis_bootstrap::render_local_nats_config(
                args.server_name.as_deref().unwrap_or("trellis"),
                &data.display().to_string(),
                "./jwt.conf",
                args.nats_port,
                args.nats_ws_port,
                args.nats_monitor_port,
                (!loopback)
                    .then(|| trellis_bootstrap::NatsTlsFiles {
                        certificate: nats.join("tls/nats.crt").display().to_string(),
                        private_key: nats.join("tls/nats.key").display().to_string(),
                        websocket_behind_proxy: args.nats_websocket_behind_proxy || relay,
                    })
                    .as_ref(),
            ),
        )
        .into_diagnostic()?;
    }
    let trellis_config = out.join("config.toml");
    let nats_config = out.join("nats/nats.conf");
    if format == OutputFormat::Json {
        print_json(&json!({
            "generated": true,
            "out": out.display().to_string(),
            "trellisConfig": trellis_config.display().to_string(),
            "natsConfig": nats_config.display().to_string(),
            "trellisRuntimeCreds": out.join("nats/creds/trellis-auth.creds").display().to_string(),
            "publicOrigin": options.runtime.public_origin,
            "natsServer": options.runtime.nats_server_url,
            "natsWebsocket": options.runtime.nats_websocket_url,
            "oauthOrigin": options.runtime.oauth_origin,
            "generatedTls": generated.as_ref().map(|tls| json!({
                "caCertificate": tls.ca_certificate.display().to_string(),
                "caSha256Fingerprint": tls.ca_sha256_fingerprint,
                "leafNotAfter": rfc3339(tls.leaf_not_after),
            })),
        }));
        return Ok(());
    }

    println!("generated Trellis bootstrap files");
    println!("out={}", out.display());
    println!("trellisConfig={}", trellis_config.display());
    println!("natsConfig={}", nats_config.display());
    println!("publicOrigin={}", options.runtime.public_origin);
    println!("natsServer={}", options.runtime.nats_server_url);
    println!("natsWebsocket={}", options.runtime.nats_websocket_url);
    println!("oauthOrigin={}", options.runtime.oauth_origin);
    if let Some(tls) = &generated {
        warn_generated_tls(
            &tls.ca_certificate,
            &tls.ca_sha256_fingerprint,
            tls.leaf_not_after,
        );
    }
    Ok(())
}

/// Resolves a `--bind-address` to the first of its addresses this machine can
/// listen on, so a hostname picks the interface it names.
fn parse_bind_address(value: &str) -> Result<std::net::IpAddr, String> {
    use std::net::ToSocketAddrs as _;

    let addresses: Vec<std::net::IpAddr> = (value, 0)
        .to_socket_addrs()
        .map_err(|error| format!("can't resolve {value}: {error}"))?
        .map(|address| address.ip())
        .collect();
    addresses
        .iter()
        .copied()
        .find(|address| std::net::TcpListener::bind((*address, 0)).is_ok())
        .ok_or_else(|| {
            let listed: Vec<String> = addresses.iter().map(ToString::to_string).collect();
            if listed == [value] {
                format!("{value} isn't an address of this machine")
            } else {
                format!(
                    "{value} resolves to {}, none of which is an address of this machine",
                    listed.join(", ")
                )
            }
        })
}

fn required_path(
    value: &Option<std::path::PathBuf>,
    flag: &str,
) -> miette::Result<std::path::PathBuf> {
    value
        .clone()
        .ok_or_else(|| miette!("{flag} is required with --tls supplied"))
}

fn rfc3339(value: time::OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| value.to_string())
}

/// Always-on warning for installations serving their generated CA.
fn warn_generated_tls(
    ca: &std::path::Path,
    fingerprint: &str,
    leaf_not_after: time::OffsetDateTime,
) {
    eprintln!(
        "warning: this installation uses certificates Trellis generated itself, which \
        suit development only.\n  Browsers warn the first time and `trellis login` asks; \
        accept only if the issuing CA ({}) has this SHA-256 fingerprint:\n  \
        {fingerprint}\n  Leaf certificates expire {}; renew them with `trellis-server \
        tls renew`.",
        ca.display(),
        rfc3339(leaf_not_after),
    );
}

pub(crate) fn tls(command: TlsSubcommand) -> miette::Result<()> {
    match command {
        TlsSubcommand::Renew(args) => {
            let format = args.format;
            let ca_dir = args
                .ca_dir
                .clone()
                .unwrap_or_else(|| trellis_bootstrap::bundle_ca_directory(&args.bundle));
            let renewed = trellis_bootstrap::renew_generated_certificates(&args.bundle, &ca_dir)
                .map_err(bootstrap_report)?;
            if format == OutputFormat::Json {
                print_json(&json!({
                    "renewed": renewed.iter().map(|certificate| json!({
                        "certificate": certificate.certificate.display().to_string(),
                        "notAfter": rfc3339(certificate.not_after),
                    })).collect::<Vec<_>>(),
                }));
                return Ok(());
            }
            for certificate in &renewed {
                println!(
                    "renewed {} (expires {})",
                    certificate.certificate.display(),
                    rfc3339(certificate.not_after)
                );
            }
            println!(
                "the runtime reloads replaced certificates automatically; reload NATS \
                (`nats-server --signal reload` or restart its unit) to serve its \
                replacement"
            );
            Ok(())
        }
        TlsSubcommand::Issue(args) => {
            let issued = trellis_bootstrap::issue_certificates(&args.out, &args.name, &args.hosts)
                .map_err(bootstrap_report)?;
            if args.format == OutputFormat::Json {
                print_json(&json!({
                    "caCertificate": issued.ca_certificate.display().to_string(),
                    "caSha256Fingerprint": issued.ca_sha256_fingerprint,
                    "certificateChain": issued.certificate_chain.display().to_string(),
                    "privateKey": issued.private_key.display().to_string(),
                    "notAfter": rfc3339(issued.not_after),
                }));
                return Ok(());
            }
            println!("caCertificate={}", issued.ca_certificate.display());
            println!("caSha256Fingerprint={}", issued.ca_sha256_fingerprint);
            println!("certificateChain={}", issued.certificate_chain.display());
            println!("privateKey={}", issued.private_key.display());
            println!("notAfter={}", rfc3339(issued.not_after));
            Ok(())
        }
        TlsSubcommand::Status(TlsStatusArgs { profile, format }) => {
            let path = profile.paths(|name| std::env::var_os(name))?.config;
            let config = trellis_runtime::RuntimeConfig::load_from_path(&path).into_diagnostic()?;
            let notice = trellis_runtime::tls::GeneratedTlsNotice::from_config(
                &config,
                config.http_transport(),
            )
            .into_diagnostic()?;
            if format == OutputFormat::Json {
                print_json(&json!({
                    "generatedTls": notice.as_ref().map(|notice| json!({
                        "caSha256Fingerprint": notice.ca.sha256_fingerprint,
                        "caNotAfter": rfc3339(notice.ca.not_after),
                        "leafNotAfter": notice.leaf.as_ref().map(|leaf| rfc3339(leaf.not_after)),
                    })),
                }));
                return Ok(());
            }
            match notice {
                Some(notice) => {
                    let ca = config
                        .tls
                        .as_ref()
                        .and_then(|tls| tls.generated_ca_certificate.clone())
                        .unwrap_or_default();
                    warn_generated_tls(
                        &ca,
                        &notice.ca.sha256_fingerprint,
                        notice
                            .leaf
                            .as_ref()
                            .map_or(notice.ca.not_after, |leaf| leaf.not_after),
                    );
                }
                None => println!("this installation does not use Trellis-generated TLS"),
            }
            Ok(())
        }
    }
}

fn bootstrap_report(error: BootstrapError) -> miette::Report {
    miette::Report::new(error)
}

fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

#[cfg(test)]
mod tests {
    use super::{init_config, InitConfigArgs, OutputFormat};
    use crate::{Args, OperationArgs};
    use clap::Parser as _;
    use std::path::Path;

    #[test]
    fn init_config_maps_bootstrap_policies_into_effective_runtime_configuration() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (config, listeners) = generate_bundle(
            &temp.path().join("custom"),
            &[
                "--trellis-port",
                "3444",
                "--nats-port",
                "4333",
                "--nats-monitor-port",
                "8333",
                "--nats-ws-port",
                "8183",
                "--bind-address",
                "localhost",
                "--auth-context-lifetime-seconds",
                "90",
                "--auth-oauth-admission-lifetime-seconds",
                "12",
                "--auth-allowed-clock-skew-seconds",
                "2",
            ],
        );
        let http = config.http.as_ref().expect("http");
        assert_eq!(
            http.public_origin.as_deref(),
            Some("https://localhost:3444")
        );
        // A hostname binds to the address it resolves to on this machine.
        assert!(http.bind_address.expect("bind address").is_loopback());
        // An address no interface holds can't be listened on.
        assert!(
            Args::try_parse_from(["trellis-server", "init", "--bind-address", "192.0.2.1"])
                .is_err()
        );
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "tls://localhost:4333"
        );
        assert_eq!(
            (listeners.native, listeners.monitor, listeners.websocket),
            (4333, 8333, 8183)
        );
        // Generated TLS relays browser WebSockets through the Console origin
        // to the broker's WebSocket listener.
        assert_eq!(
            http.nats_websocket_upstream.as_deref(),
            Some("localhost:8183")
        );
        assert_eq!(
            config.same_origin_websocket_url().as_deref(),
            Some("wss://localhost:3444/nats")
        );
        let auth = config.resolve_authorization().unwrap();
        assert_eq!(auth.context_lifetime_seconds, 90);
        assert_eq!(auth.oauth_admission_lifetime_seconds, 12);
        assert_eq!(auth.allowed_clock_skew_seconds, 2);
    }

    #[test]
    fn init_config_explicit_advertised_urls_override_derived_values() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (config, _) = generate_bundle(
            &temp.path().join("explicit"),
            &[
                "--nats-port",
                "4333",
                "--nats-server-url",
                "tls://nats.example.test:4999",
                "--nats-websocket-url",
                "wss://nats.example.test/ws",
                "--public-origin",
                "https://trellis.example.test",
            ],
        );
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "tls://nats.example.test:4999"
        );
        assert_eq!(
            config
                .client
                .as_ref()
                .unwrap()
                .ws_nats_servers
                .as_ref()
                .unwrap(),
            &["wss://nats.example.test/ws"]
        );
        assert_eq!(
            config.http.as_ref().unwrap().public_origin.as_deref(),
            Some("https://trellis.example.test")
        );
    }

    #[test]
    fn init_config_adds_extra_origins() {
        let temp = tempfile::tempdir().expect("temp dir");
        let (config, _) = generate_bundle(
            &temp.path().join("extra-origin"),
            &["--extra-origin", "http://localhost:5174"],
        );
        let http = config.http.unwrap();
        assert!(http
            .origins
            .unwrap()
            .iter()
            .any(|origin| origin == "http://localhost:5174"));
    }

    #[test]
    fn init_config_rejects_zero_duplicate_and_trellis_collisions_without_writing() {
        let temp = tempfile::tempdir().expect("temp dir");
        for (name, flags) in [
            ("zero", vec!["--nats-port", "0"]),
            (
                "duplicate",
                vec!["--nats-port", "4333", "--nats-monitor-port", "4333"],
            ),
            ("collision", vec!["--nats-port", "3000"]),
        ] {
            let out = temp.path().join(name);
            let args = init_config_args(&out, &flags);
            init_config(OutputFormat::Json, &args, false, out.clone())
                .expect_err("invalid listener ports must be rejected");
            assert!(
                !out.exists(),
                "{name}: rejected configuration must not write output"
            );
        }
    }

    fn init_config_args(out: &Path, flags: &[&str]) -> InitConfigArgs {
        let out = out.to_str().expect("UTF-8 output path").to_string();
        let mut arguments = vec![
            "trellis-server".to_string(),
            "init".to_string(),
            "--out".to_string(),
            out,
            "--tls".to_string(),
            "generated".to_string(),
        ];
        arguments.extend(flags.iter().map(|flag| (*flag).to_string()));
        match Args::parse_from(&arguments).operation {
            Some(OperationArgs::Init(init)) => init.config,
            other => panic!("unexpected command: {other:?}"),
        }
    }

    fn generate_bundle(
        out: &Path,
        flags: &[&str],
    ) -> (
        trellis_runtime::RuntimeConfig,
        trellis_bootstrap::NatsListeners,
    ) {
        let args = init_config_args(out, flags);
        init_config(OutputFormat::Json, &args, false, out.to_path_buf()).expect("generate bundle");
        (
            trellis_runtime::RuntimeConfig::load_from_path(out.join("config.toml"))
                .expect("load production runtime config"),
            trellis_bootstrap::read_nats_listen_ports(&out.join("nats/nats.conf"))
                .expect("resolve native listener configuration"),
        )
    }
}
