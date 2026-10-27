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
    /// Generate an installation CA and short-lived certificates (default).
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
    /// Output directory for generated Trellis bootstrap files.
    // Required, but checked when generating: clap would otherwise demand it
    // from `init admin` whenever a global option such as --config follows.
    pub(crate) out: Option<PathBuf>,

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

    #[arg(long, value_enum, default_value_t = InitTlsMode::Generated)]
    /// Certificate source. Generated TLS creates an installation CA that
    /// browsers and native clients must trust explicitly.
    pub(crate) tls: InitTlsMode,

    #[arg(long)]
    /// Directory receiving the generated CA private key; defaults to
    /// `<out>-tls-ca`. Keep it out of every runtime and broker mount.
    pub(crate) tls_ca_dir: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
    /// Supplied PEM certificate chain naming the Console and OAuth hosts.
    pub(crate) tls_certificate_chain: Option<PathBuf>,

    #[arg(long, required_if_eq("tls", "supplied"))]
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
    /// HTTP listener IP address.
    #[arg(long)]
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
    /// Show the installation CA fingerprint and certificate expiry for --config.
    Status,
}

#[derive(Debug, clap::Args)]
/// Renew generated leaf certificates without changing client trust.
pub(crate) struct TlsRenewArgs {
    /// Bundle directory written by `trellis-server init --out`.
    #[arg(long)]
    pub(crate) bundle: PathBuf,
    /// CA directory holding the private key; defaults to `<bundle>-tls-ca`.
    #[arg(long)]
    pub(crate) ca_dir: Option<PathBuf>,
}

pub(crate) fn init_config(format: OutputFormat, args: &InitConfigArgs) -> miette::Result<()> {
    let out = args
        .out
        .clone()
        .ok_or_else(|| miette!("trellis-server init needs --out <DIR>"))?;
    let mut options = TrellisBootstrapOptions::new(out.clone());
    options.force = args.force;
    options.runtime.name = args.name.clone();
    options.runtime.trellis_port = args.trellis_port;
    let loopback = args.tls == InitTlsMode::LoopbackDevelopment;
    let (http, native, websocket) = if loopback {
        ("http", "nats", "ws")
    } else {
        ("https", "tls", "wss")
    };
    options.runtime.nats_server_url = args
        .nats_server_url
        .clone()
        .unwrap_or_else(|| format!("{native}://localhost:{}", args.nats_port));
    options.runtime.nats_websocket_url = args
        .nats_websocket_url
        .clone()
        .unwrap_or_else(|| format!("{websocket}://localhost:{}", args.nats_ws_port));
    options.runtime.public_origin = args
        .public_origin
        .clone()
        .unwrap_or_else(|| format!("{http}://localhost:{}", args.trellis_port));
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
            args.tls == InitTlsMode::Generated
                && args.nats_websocket_url.is_none()
                && !args.nats_websocket_behind_proxy
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
    options.runtime.oauth_origin = args
        .oauth_origin
        .clone()
        .unwrap_or_else(|| format!("{http}://localhost:{}", args.oauth_port));
    options.runtime.oauth_port = args.oauth_port;
    options.runtime.tls = match args.tls {
        InitTlsMode::Generated => BootstrapTls::Generated {
            ca_directory: args.tls_ca_dir.clone(),
        },
        InitTlsMode::Supplied => BootstrapTls::Supplied(SuppliedTls {
            certificate_chain: required_path(
                &args.tls_certificate_chain,
                "--tls-certificate-chain",
            )?,
            private_key: required_path(&args.tls_private_key, "--tls-private-key")?,
            nats_certificate_chain: required_path(
                &args.nats_tls_certificate_chain,
                "--nats-tls-certificate-chain",
            )?,
            nats_private_key: required_path(&args.nats_tls_private_key, "--nats-tls-private-key")?,
            ca_certificate: args.tls_ca_certificate.clone(),
        }),
        InitTlsMode::LoopbackDevelopment => BootstrapTls::LoopbackDevelopment,
    };
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
        "warning: this installation uses Trellis-generated TLS from its own private \
        CA.\n  Browsers and native clients must trust {} explicitly.\n  Verify \
        this SHA-256 fingerprint out of band before trusting it:\n  \
        {fingerprint}\n  Leaf certificates expire {}; renew them with `trellis-server \
        tls renew`.",
        ca.display(),
        rfc3339(leaf_not_after),
    );
}

pub(crate) fn tls(
    format: OutputFormat,
    command: TlsSubcommand,
    config: Option<PathBuf>,
) -> miette::Result<()> {
    match command {
        TlsSubcommand::Renew(args) => {
            let ca_dir = args.ca_dir.clone().unwrap_or_else(|| {
                let mut name = args
                    .bundle
                    .file_name()
                    .map(std::ffi::OsStr::to_os_string)
                    .unwrap_or_else(|| "trellis".into());
                name.push("-tls-ca");
                args.bundle.with_file_name(name)
            });
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
        TlsSubcommand::Status => {
            let path = config.ok_or_else(|| miette!("tls status needs --config"))?;
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
                "127.0.0.1",
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
        assert_eq!(http.bind_address, Some("127.0.0.1".parse().unwrap()));
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
            init_config(OutputFormat::Json, &args)
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
        init_config(OutputFormat::Json, &args).expect("generate bundle");
        (
            trellis_runtime::RuntimeConfig::load_from_path(out.join("config.toml"))
                .expect("load production runtime config"),
            trellis_bootstrap::read_nats_listen_ports(&out.join("nats/nats.conf"))
                .expect("resolve native listener configuration"),
        )
    }
}
