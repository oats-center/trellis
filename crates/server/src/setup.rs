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
    /// On the Trellis origin: the Trellis port sends every WebSocket upgrade
    /// to NATS, as a proxy in front would, so clients trust one host and
    /// certificate. Default with generated TLS.
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
    /// Your HTTPS proxy holds every certificate. Trellis and the NATS
    /// WebSocket serve plaintext to the --trusted-proxy networks, and Trellis
    /// reaches NATS without TLS on this machine's loopback.
    Proxy,
    /// Plaintext restricted to loopback origins and listeners.
    LoopbackDevelopment,
}

#[derive(Debug, clap::Args)]
/// Generate Trellis runtime config and NATS bootstrap material.
pub(crate) struct InitConfigArgs {
    #[arg(long)]
    /// Connect to a NATS server you run from the generated files instead of
    /// having trellis-server start one.
    pub(crate) external_nats: bool,

    #[arg(long)]
    /// Also write Podman Quadlet units to the installation's `quadlet` folder,
    /// running Trellis and NATS as two rootless containers that use the
    /// folder in place. Implies --external-nats; NATS needs a hostname other
    /// than localhost, which the containers reach each other by.
    pub(crate) quadlet: bool,

    #[arg(long)]
    /// Development mode: generated certificates, and the Trellis port relays
    /// NATS WebSockets, so a machine elsewhere accepts one certificate. Not
    /// for production.
    pub(crate) dev: bool,

    #[arg(long, default_value = "localhost")]
    /// Hostname the default Trellis and NATS addresses use.
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

    #[arg(long, default_value_t = trellis_bootstrap::DEFAULT_TRELLIS_PORT)]
    /// Trellis port: sign-in, discovery and health.
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
    /// Where Trellis itself reaches NATS; defaults to tls://localhost:<nats-port>
    /// (nats:// in loopback development and with --tls proxy). A wss:// URL
    /// reaches a NATS that trellis-server doesn't run through your proxy's
    /// WebSocket, like every other client.
    pub(crate) nats_server_url: Option<String>,

    #[arg(long)]
    /// Browser-facing NATS websocket URL for Trellis clients; defaults to
    /// wss://<host>:<nats-ws-port> (ws:// in loopback development), or the
    /// broker's own host for an external broker on another machine.
    pub(crate) nats_websocket_url: Option<String>,

    #[arg(long)]
    /// Public Trellis origin, where sign-in lives and clients connect;
    /// defaults to https://<host>:<trellis-port> (http:// in loopback
    /// development).
    pub(crate) trellis_origin: Option<String>,

    #[arg(long)]
    /// Give the Console an origin of its own, another hostname that reaches
    /// the Trellis port, so its pages run apart from sign-in. Without it the
    /// Console is at /console on the Trellis origin.
    pub(crate) console_origin: Option<String>,

    #[arg(long, value_enum)]
    /// Certificate source. Required without --dev, which generates them.
    pub(crate) tls: Option<InitTlsMode>,

    #[arg(long)]
    /// Directory receiving the generated CA private key; defaults to the
    /// installation's `tls-ca` folder, or `/etc/trellis-tls-ca` with
    /// `--system`.
    /// Keep it out of every runtime and broker mount.
    pub(crate) tls_ca_dir: Option<PathBuf>,

    #[arg(long, requires = "tls_private_key")]
    /// Supplied PEM certificate chain naming the Trellis host (and the
    /// Console's, with --console-origin);
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
    /// Serve the Trellis port as plaintext behind an HTTPS-terminating
    /// reverse proxy at this network; repeatable. Public origins stay HTTPS.
    pub(crate) trusted_proxy: Vec<ipnet::IpNet>,

    #[arg(long)]
    /// Serve NATS WebSockets as plaintext behind an HTTPS-terminating reverse
    /// proxy. Native NATS always terminates its own TLS.
    pub(crate) nats_websocket_behind_proxy: bool,

    #[arg(long)]
    /// Advertise only the NATS WebSocket, so every client connects through it
    /// and native NATS stays private. --nats-server-url is then only where
    /// Trellis itself reaches NATS.
    pub(crate) nats_websocket_only: bool,

    #[arg(long, value_enum)]
    /// How clients reach NATS WebSockets. `relay` has the Trellis port send
    /// every WebSocket upgrade to NATS, so one certificate covers both; the
    /// broker's WebSocket port then serves plaintext and must stay private.
    /// Defaults to `relay` unless --tls loopback-development,
    /// --nats-websocket-url or --nats-websocket-behind-proxy is given, and to
    /// `direct` otherwise. A broker that --external-nats reaches on another
    /// host can't be relayed, since the relay's hop to it is plaintext.
    pub(crate) nats_websocket: Option<InitWebsocketMode>,

    /// Address Trellis listens on: an IP or a hostname that
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
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// CA directory holding the private key; defaults to the installation's
    /// `tls-ca` folder, else the `-tls-ca` folder beside it.
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
    pub(crate) profile: crate::InstallationArgs,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

pub(crate) fn init_config(
    format: OutputFormat,
    args: &InitConfigArgs,
    location: &crate::Location,
    out: PathBuf,
) -> miette::Result<()> {
    let system = *location == crate::Location::System;
    if args.quadlet && !matches!(location, crate::Location::Bundle(_)) {
        return Err(miette!(
            "--quadlet writes the units into the installation folder: \
            trellis-server init <folder> --quadlet"
        ));
    }
    if args.quadlet && args.nats_data_dir.is_some() {
        return Err(miette!(
            "--quadlet keeps NATS data in the installation's var/nats; drop --nats-data-dir"
        ));
    }
    let mut options = TrellisBootstrapOptions::new(out.clone());
    options.force = args.force;
    options.runtime.name = args.name.clone();
    options.runtime.trellis_port = args.trellis_port;
    let tls = match (args.tls, args.dev) {
        (Some(tls), _) => tls,
        (None, true) => InitTlsMode::Generated,
        (None, false) => {
            return Err(miette!(
                "choose where this installation's certificates come from: --tls proxy when \
                your HTTPS proxy holds them all, --tls supplied with certificates browsers \
                already trust, --tls generated for a lab or test installation, or --dev for \
                development"
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
            "--dev relays NATS WebSockets through the Trellis port; it can't be combined \
            with --tls loopback-development, --nats-websocket direct or --nats-websocket-url"
        ));
    }
    let host = &args.host;
    let proxy = tls == InitTlsMode::Proxy;
    let (http, native, websocket) = if loopback {
        ("http", "nats", "ws")
    } else if proxy {
        ("https", "nats", "wss")
    } else {
        ("https", "tls", "wss")
    };
    // Each Quadlet container has a loopback of its own, so Trellis reaches
    // NATS by its container's name on the installation's network. Otherwise
    // plaintext NATS stays on loopback, whatever --host names.
    let quadlet_name = args.quadlet.then(|| quadlet_name(&out)).transpose()?;
    let nats_host = match &quadlet_name {
        Some(name) => format!("{name}-nats"),
        None if proxy => "localhost".to_owned(),
        None => host.clone(),
    };
    options.runtime.nats_server_url = args
        .nats_server_url
        .clone()
        .unwrap_or_else(|| format!("{native}://{nats_host}:{}", args.nats_port));
    options.runtime.public_origin = args
        .trellis_origin
        .clone()
        .unwrap_or_else(|| format!("{http}://{host}:{}", args.trellis_port));
    // The relay reaches the broker's WebSocket in plaintext, so only on this
    // machine or a Quadlet's own network; a broker elsewhere serves TLS itself.
    let remote_nats = (args.external_nats && !args.quadlet)
        .then(|| remote_host(&options.runtime.nats_server_url))
        .flatten();
    // Browsers reach a broker elsewhere at its own address.
    let websocket_host = remote_nats.as_deref().unwrap_or(host);
    options.runtime.nats_websocket_url = args
        .nats_websocket_url
        .clone()
        .unwrap_or_else(|| format!("{websocket}://{websocket_host}:{}", args.nats_ws_port));
    let remote_nats = remote_nats.is_some();
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
        // Out of the box, a browser that trusts the Console's certificate
        // also reaches NATS; a separate WebSocket port would need its own
        // certificate and hostname.
        None => {
            args.dev
                || (!loopback
                    && !remote_nats
                    && args.nats_websocket_url.is_none()
                    && !args.nats_websocket_behind_proxy)
        }
    };
    if relay && remote_nats {
        return Err(miette!(
            "relaying NATS WebSockets would send NATS traffic unencrypted to the external \
            broker at {}; relay only to a broker on this machine, or pass \
            --nats-websocket direct",
            options.runtime.nats_server_url
        ));
    }
    if relay {
        // Reported for operators; the runtime derives the same URL itself.
        options.runtime.nats_websocket_url = options
            .runtime
            .public_origin
            .trim_end_matches('/')
            .replacen("http", "ws", 1);
    }
    options.runtime.console_origin = args.console_origin.clone();
    options.runtime.tls = match tls {
        // A system installation is the one roles mount, and the containers
        // mount a Quadlet installation's folder, so their CA stays beside it.
        InitTlsMode::Generated => BootstrapTls::Generated {
            ca_directory: args.tls_ca_dir.clone().or_else(|| {
                (!system && !args.quadlet).then(|| out.join(trellis_bootstrap::BUNDLE_CA_DIRECTORY))
            }),
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
        InitTlsMode::Proxy => BootstrapTls::Proxy,
        InitTlsMode::LoopbackDevelopment => BootstrapTls::LoopbackDevelopment,
    };
    options.runtime.managed_nats = !args.external_nats && !args.quadlet;
    options.runtime.trusted_proxies = args.trusted_proxy.clone();
    let quadlet = match &quadlet_name {
        Some(name) => Some(quadlet_plan(
            name,
            &args.trusted_proxy,
            &options.runtime.nats_server_url,
        )?),
        None => None,
    };
    if let Some(plan) = &quadlet {
        // Rootless Podman delivers every published connection from the
        // network's own address, so behind a proxy that network is what
        // Trellis accepts; the firewall decides who reaches the ports.
        if proxy || !args.trusted_proxy.is_empty() {
            options.runtime.trusted_proxies = vec![ipnet::IpNet::V4(plan.subnet)];
        }
        options.runtime.nats_private_network = proxy;
    }
    options.runtime.nats_websocket_behind_proxy = args.nats_websocket_behind_proxy;
    options.runtime.nats_websocket_via_runtime = relay;
    options.runtime.nats_websocket_only = args.nats_websocket_only;
    // In a container Trellis listens everywhere; --bind-address is then
    // where the host publishes its port.
    options.runtime.bind_address = if args.quadlet {
        Some(std::net::Ipv4Addr::UNSPECIFIED.into())
    } else {
        args.bind_address
    };

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
        std::fs::create_dir_all(data.join("jwt")).into_diagnostic()?;
        let authored = std::fs::read_to_string(out.join("nats.conf")).into_diagnostic()?;
        std::fs::write(
            out.join("nats.conf"),
            trellis_bootstrap::render_local_nats_config(
                args.server_name.as_deref().unwrap_or("trellis"),
                &data.display().to_string(),
                &trellis_bootstrap::nats_accounts(
                    &authored,
                    &data.join("jwt").display().to_string(),
                ),
                trellis_bootstrap::NatsListeners {
                    native: args.nats_port,
                    monitor: args.nats_monitor_port,
                    websocket: args.nats_ws_port,
                },
                (!loopback && !proxy)
                    .then(|| trellis_bootstrap::NatsTlsFiles {
                        certificate: out.join("tls/nats.crt").display().to_string(),
                        private_key: out.join("tls/nats.key").display().to_string(),
                    })
                    .as_ref(),
                if relay {
                    trellis_bootstrap::NatsWebsocket::Relayed
                } else if args.nats_websocket_behind_proxy {
                    trellis_bootstrap::NatsWebsocket::BehindProxy
                } else if loopback {
                    trellis_bootstrap::NatsWebsocket::Loopback
                } else {
                    trellis_bootstrap::NatsWebsocket::Tls
                },
            ),
        )
        .into_diagnostic()?;
    }
    if let Some(plan) = &quadlet {
        write_quadlet(plan, args, relay, loopback || proxy, &out)?;
    }
    let trellis_config = out.join("config.toml");
    let nats_config = out.join("nats.conf");
    // How to start what was just written.
    let start = match location {
        crate::Location::User => "trellis-server".to_owned(),
        crate::Location::System => "trellis-server --system".to_owned(),
        crate::Location::Bundle(_) => format!("trellis-server --config {}", out.display()),
    };
    let console = format!(
        "{}/console/",
        options
            .runtime
            .console_origin
            .as_deref()
            .unwrap_or(&options.runtime.public_origin)
            .trim_end_matches('/')
    );
    if format == OutputFormat::Json {
        print_json(&json!({
            "generated": true,
            "out": out.display().to_string(),
            "trellisConfig": trellis_config.display().to_string(),
            "natsConfig": nats_config.display().to_string(),
            "start": start,
            "quadlet": quadlet.as_ref().map(|_| out.join("quadlet").display().to_string()),
            "trellisOrigin": options.runtime.public_origin,
            "natsServer": options.runtime.nats_server_url,
            "natsWebsocket": options.runtime.nats_websocket_url,
            "console": console,
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
    println!("trellisOrigin={}", options.runtime.public_origin);
    println!("natsServer={}", options.runtime.nats_server_url);
    println!("natsWebsocket={}", options.runtime.nats_websocket_url);
    println!("console={console}");
    println!("start={start}");
    if quadlet.is_some() {
        println!("quadlet={}", out.join("quadlet").display());
    }
    if let Some(tls) = &generated {
        warn_generated_tls(
            &tls.ca_certificate,
            &tls.ca_sha256_fingerprint,
            tls.leaf_not_after,
            !options.runtime.trusted_proxies.is_empty(),
        );
    }
    Ok(())
}

/// What the Quadlet units are named, the network they share, and the NATS
/// hostname the network resolves to the NATS container.
struct QuadletPlan {
    name: String,
    subnet: ipnet::Ipv4Net,
    nats_alias: Option<String>,
}

/// The units take the installation folder's name.
fn quadlet_name(out: &std::path::Path) -> miette::Result<String> {
    let name = std::path::absolute(out)
        .into_diagnostic()?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(miette!(
            "--quadlet names the units after the installation folder, so its name may \
            only use letters, digits, '-', '_' and '.'"
        ));
    }
    Ok(name)
}

/// Checks that the installation can run as containers before anything is
/// written.
fn quadlet_plan(
    name: &str,
    trusted_proxies: &[ipnet::IpNet],
    nats_server_url: &str,
) -> miette::Result<QuadletPlan> {
    let nats = url::Url::parse(nats_server_url)
        .map_err(|_| miette!("'{nats_server_url}' is not an absolute URL"))?;
    let host = nats.host_str().unwrap_or_default().to_owned();
    // Through the proxy, Trellis needs no route to the NATS container.
    let nats_alias = if nats.scheme() == "wss" || host == format!("{name}-nats") {
        None
    } else if remote_host(nats_server_url).is_none() {
        return Err(miette!(
            "with --quadlet, Trellis reaches NATS in another container, which \
            '{nats_server_url}' can't name; leave out --nats-server-url to use the \
            container's name"
        ));
    } else {
        // The name the operator's NATS certificate carries.
        Some(host)
    };
    // A private range of the installation's own, chosen from its name so
    // several installations on one host differ, and never one a proxy is in.
    let start = name.bytes().fold(0u8, |sum, byte| sum.wrapping_add(byte));
    let subnet = (0..=255u8)
        .map(|offset| {
            ipnet::Ipv4Net::new(
                std::net::Ipv4Addr::new(10, 89, start.wrapping_add(offset), 0),
                24,
            )
            .expect("a /24 prefix is valid")
        })
        .find(|subnet| {
            trusted_proxies.iter().all(|proxy| match proxy {
                ipnet::IpNet::V4(proxy) => {
                    !proxy.contains(&subnet.network()) && !subnet.contains(&proxy.network())
                }
                ipnet::IpNet::V6(_) => true,
            })
        })
        .ok_or_else(|| miette!("no private network range is free of the --trusted-proxy ranges"))?;
    Ok(QuadletPlan {
        name: name.to_owned(),
        subnet,
        nats_alias,
    })
}

/// Writes `<installation>/quadlet`: Trellis and NATS as rootless containers
/// on a network of their own, using the installation folder in place, with
/// its `var` folder writable.
fn write_quadlet(
    plan: &QuadletPlan,
    args: &InitConfigArgs,
    relay: bool,
    plaintext: bool,
    out: &std::path::Path,
) -> miette::Result<()> {
    let folder = std::path::absolute(out).into_diagnostic()?;
    for directory in [folder.join("var"), folder.join("var/nats")] {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&directory).into_diagnostic()?;
    }
    let folder = folder.display();
    let name = &plan.name;
    let published = |address: Option<std::net::IpAddr>, port: u16| match address {
        Some(std::net::IpAddr::V6(ip)) => format!("PublishPort=[{ip}]:{port}:{port}\n"),
        Some(ip) => format!("PublishPort={ip}:{port}:{port}\n"),
        None => format!("PublishPort={port}:{port}\n"),
    };
    let local = Some(std::net::Ipv4Addr::LOCALHOST.into());
    // Plaintext behind a proxy is published only where --bind-address says,
    // loopback by default, so it never reaches the network by accident.
    let proxied = |behind: bool| {
        if behind {
            args.bind_address.or(local)
        } else {
            args.bind_address
        }
    };
    let behind_proxy = !args.trusted_proxy.is_empty() || plaintext;
    let hardening = "UserNS=keep-id:uid=10001,gid=10001\nNoNewPrivileges=true\n\
        DropCapability=all\nReadOnly=true\nTmpfs=/tmp:rw,noexec,nosuid,nodev,size=16m\n";
    let service = "\n[Service]\nRestart=on-failure\nRestartSec=5\n\n\
        [Install]\nWantedBy=default.target\n";

    let trellis = format!(
        "[Unit]\nDescription=Trellis ({name})\nRequires={name}-nats.service\n\
        After={name}-nats.service\n\n[Container]\nContainerName={name}\n\
        Image=ghcr.io/oats-center/trellis-server:v{version}\nExec=--config /etc/trellis\n\
        Volume={folder}:/etc/trellis:ro,z\nVolume={folder}/var:/etc/trellis/var:rw,z\n\
        {publish}Network={name}.network\n{hardening}{service}",
        version = env!("CARGO_PKG_VERSION"),
        publish = published(proxied(behind_proxy), args.trellis_port),
    );
    // With only the WebSocket offered, native NATS stays on the network.
    let mut nats_publish = if args.nats_websocket_only {
        String::new()
    } else {
        published(args.bind_address, args.nats_port)
    };
    if !relay {
        nats_publish.push_str(&published(
            proxied(args.nats_websocket_behind_proxy),
            args.nats_ws_port,
        ));
    }
    nats_publish.push_str(&published(local, args.nats_monitor_port));
    let certificates = if plaintext {
        String::new()
    } else {
        format!(
            "Volume={folder}/tls/nats.crt:/etc/nats/tls/nats.crt:ro,z\n\
            Volume={folder}/tls/nats.key:/etc/nats/tls/nats.key:ro,z\n"
        )
    };
    let alias = plan
        .nats_alias
        .as_ref()
        .map(|alias| format!("NetworkAlias={alias}\n"))
        .unwrap_or_default();
    let nats = format!(
        "[Unit]\nDescription=NATS for Trellis ({name})\nAfter=network-online.target\n\
        Wants=network-online.target\n\n[Container]\nContainerName={name}-nats\n\
        Image=docker.io/library/nats:{version}-alpine\nExec=-c /etc/nats/nats.conf\n\
        Volume={folder}/nats.conf:/etc/nats/nats.conf:ro,z\n{certificates}\
        Volume={folder}/var/nats:/data:rw,z\n{nats_publish}Network={name}.network\n\
        {alias}{hardening}{service}",
        version = trellis_local_nats::pinned_version().into_diagnostic()?,
        hardening = hardening.replace("keep-id:uid=10001,gid=10001", "keep-id"),
    );
    let network = format!(
        "[Network]\n# Rootless Podman delivers published connections from this \
        network, which config.toml trusts as the proxy.\nSubnet={}\n",
        plan.subnet
    );
    let directory = out.join("quadlet");
    std::fs::create_dir_all(&directory).into_diagnostic()?;
    for (file, contents) in [
        (format!("{name}.container"), trellis),
        (format!("{name}-nats.container"), nats),
        (format!("{name}.network"), network),
    ] {
        std::fs::write(directory.join(file), contents).into_diagnostic()?;
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

/// Always-on warning for installations serving their generated CA. Behind a
/// proxy, browsers get the proxy's certificate and only NATS serves this one.
fn warn_generated_tls(
    ca: &std::path::Path,
    fingerprint: &str,
    leaf_not_after: time::OffsetDateTime,
    behind_proxy: bool,
) {
    if behind_proxy {
        eprintln!(
            "note: Trellis generated a CA ({}) for NATS. The Console and sign-in use your \
            proxy's certificate, so browsers never see it.\n  Anything that connects to \
            native NATS directly accepts it only with this SHA-256 fingerprint:\n  \
            {fingerprint}\n  Leaf certificates expire {}; renew them with `trellis-server \
            tls renew`.",
            ca.display(),
            rfc3339(leaf_not_after),
        );
        return;
    }
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
            let bundle = args
                .profile
                .paths(|name| std::env::var_os(name))?
                .config_root;
            let ca_dir = args
                .ca_dir
                .clone()
                .unwrap_or_else(|| trellis_bootstrap::bundle_ca_directory(&bundle));
            let renewed = trellis_bootstrap::renew_generated_certificates(&bundle, &ca_dir)
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
            let paths = profile.paths(|name| std::env::var_os(name))?;
            let (config, _) = trellis_runtime::RuntimeConfig::load_from_path_with_defaults(
                &paths.config,
                paths.defaults(),
            )
            .into_diagnostic()?;
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
                        matches!(
                            config.http_transport(),
                            trellis_runtime::ListenerTransport::Proxy { .. }
                        ),
                    );
                }
                None => println!("this installation does not use Trellis-generated TLS"),
            }
            Ok(())
        }
    }
}

/// Which NATS user `trellis-server nats` connects as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum NatsRole {
    /// The system account: servers, accounts and connections.
    System,
    /// The Trellis account, where services and clients run: streams, KV and
    /// subjects.
    Trellis,
    /// The auth account, which answers auth callouts.
    Auth,
}

#[derive(Debug, clap::Args)]
/// Run the `nats` CLI as one of the installation's NATS users.
pub(crate) struct NatsArgs {
    /// An installation folder from `init <DIR>`, or the config.toml in
    /// it. Omitted, the user's installation, or the system's for root on
    /// Linux.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["user", "system"])]
    pub(crate) config: Option<PathBuf>,
    /// Use the user's installation (~/.config/trellis), even as root.
    #[arg(long, conflicts_with = "system")]
    pub(crate) user: bool,
    /// Use the system installation (/etc/trellis), Linux only.
    #[arg(long)]
    pub(crate) system: bool,
    /// Print the user's creds file instead of running `nats`, for a saved
    /// `nats context` or another client.
    #[arg(long, conflicts_with = "args")]
    pub(crate) print: bool,
    /// The NATS user to connect as.
    #[arg(value_enum)]
    pub(crate) role: NatsRole,
    /// Arguments for the `nats` CLI, such as `stream ls`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

/// Runs `nats` with the installation's address, CA and the role's creds.
/// The creds exist on disk only while `nats` runs, in the installation's
/// private runtime folder.
pub(crate) fn nats(args: NatsArgs) -> miette::Result<()> {
    let location = match &args.config {
        Some(path) => crate::Location::Bundle(path.clone()),
        None => crate::Location::profile(args.user, args.system),
    };
    let paths = crate::ServerPaths::resolve(&location, |name| std::env::var_os(name))?;
    let (config, roots) = trellis_runtime::RuntimeConfig::load_from_path_with_defaults(
        &paths.config,
        paths.defaults(),
    )
    .into_diagnostic()?;
    let users = &config.secrets.nats.users;
    let (user, name) = match args.role {
        NatsRole::System => (users.system.as_ref(), "system"),
        NatsRole::Trellis => (users.trellis.as_ref(), "trellis"),
        NatsRole::Auth => (users.auth.as_ref(), "auth"),
    };
    let creds = user
        .ok_or_else(|| miette!("{} has no nats.users.{name}", trellis_runtime::SECRETS_FILE))?
        .creds();
    if args.print {
        print!("{creds}");
        return Ok(());
    }
    let nats = config
        .nats
        .as_ref()
        .ok_or_else(|| miette!("the config has no [nats]"))?;
    let servers = nats
        .servers
        .clone()
        .ok_or_else(|| miette!("the config has no [nats] servers"))?;
    crate::prepare_directory(&roots.runtime)?;
    let file = roots
        .runtime
        .join(format!("nats-{name}-{}.creds", std::process::id()));
    write_private(&file, &creds)?;
    let mut command = std::process::Command::new("nats");
    command
        .args(&args.args)
        .env("NATS_URL", servers)
        .env("NATS_CREDS", &file)
        .env_remove("NATS_CONTEXT");
    if let Some(ca) = &nats.ca_certificate {
        command.env("NATS_CA", ca);
    }
    let status = command.status();
    let _ = std::fs::remove_file(&file);
    let status = status.map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => miette!(
            "the nats CLI is not on PATH; install it (https://github.com/nats-io/natscli) or \
            use --print for the creds"
        ),
        _ => miette!("cannot run nats: {error}"),
    })?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn write_private(path: &std::path::Path, contents: &str) -> miette::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .into_diagnostic()
}

fn bootstrap_report(error: BootstrapError) -> miette::Report {
    miette::Report::new(error)
}

fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

/// The host of `url` when it isn't this machine, as a URL host (IPv6 in
/// brackets).
pub(crate) fn remote_host(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?;
    let bare = host.trim_matches(['[', ']']);
    (bare != "localhost"
        && !bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()))
    .then(|| host.to_owned())
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
        // Generated TLS relays WebSockets through the Trellis origin to the
        // broker's WebSocket listener.
        assert_eq!(
            http.nats_websocket_upstream.as_deref(),
            Some("localhost:8183")
        );
        assert_eq!(
            config.same_origin_websocket_url().as_deref(),
            Some("wss://localhost:3444")
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
                "--trellis-origin",
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
            init_config(OutputFormat::Json, &args, &bundle(&out), out.clone())
                .expect_err("invalid listener ports must be rejected");
            assert!(
                !out.exists(),
                "{name}: rejected configuration must not write output"
            );
        }
    }

    fn bundle(out: &Path) -> crate::Location {
        crate::Location::Bundle(out.to_path_buf())
    }

    fn init_config_args(out: &Path, flags: &[&str]) -> InitConfigArgs {
        let out = out.to_str().expect("UTF-8 output path").to_string();
        let mut arguments = vec![
            "trellis-server".to_string(),
            "init".to_string(),
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
        init_config(OutputFormat::Json, &args, &bundle(out), out.to_path_buf())
            .expect("generate bundle");
        (
            trellis_runtime::RuntimeConfig::load_from_path(out.join("config.toml"))
                .expect("load production runtime config"),
            trellis_bootstrap::read_nats_listen_ports(&out.join("nats.conf"))
                .expect("resolve native listener configuration"),
        )
    }

    #[test]
    fn an_external_broker_elsewhere_serves_its_own_tls_websocket() {
        let temp = tempfile::tempdir().expect("temp dir");
        let remote = [
            "--external-nats",
            "--host",
            "trellis.example.test",
            "--nats-server-url",
            "tls://nats.example.test:4222",
        ];
        // The relay's hop to the broker is plaintext, and this one crosses
        // the network.
        let out = temp.path().join("relayed");
        let relayed = init_config_args(
            &out,
            &[&remote[..], &["--nats-websocket", "relay"]].concat(),
        );
        assert!(init_config(OutputFormat::Json, &relayed, &bundle(&out), out.clone()).is_err());

        // Browsers are sent to the broker's own WebSocket, not this machine.
        let (config, _) = generate_bundle(&temp.path().join("remote"), &remote);
        assert_eq!(config.nats_websocket_upstream(), None);
        assert_eq!(
            config.client.as_ref().unwrap().ws_nats_servers.as_deref(),
            Some(&["wss://nats.example.test:8080".to_owned()][..])
        );
        let nats = std::fs::read_to_string(temp.path().join("remote/nats.conf")).unwrap();
        assert!(
            !nats.contains("no_tls"),
            "the broker's WebSocket is plaintext:\n{nats}"
        );

        // A broker on this machine is relayed as before.
        let (local, _) = generate_bundle(
            &temp.path().join("local"),
            &[
                "--external-nats",
                "--nats-server-url",
                "tls://localhost:4222",
            ],
        );
        assert!(local.nats_websocket_upstream().is_some());
    }

    #[test]
    fn quadlet_units_run_the_installation_in_place_without_its_ca_key() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("lab");
        let (config, _) = generate_bundle(&out, &["--dev", "--quadlet"]);
        let nats = config.nats.as_ref().expect("nats");
        assert!(!nats.managed, "NATS runs in its own container");
        let mut mounted = Vec::new();
        let mut nats_names = Vec::new();
        for unit in ["lab.container", "lab-nats.container"] {
            let text = std::fs::read_to_string(out.join("quadlet").join(unit)).expect("unit");
            for line in text.lines() {
                if let Some(volume) = line.strip_prefix("Volume=") {
                    let source = std::path::PathBuf::from(volume.split(':').next().unwrap());
                    assert!(
                        source.exists(),
                        "{unit} mounts missing {}",
                        source.display()
                    );
                    mounted.push(source);
                }
                if unit == "lab-nats.container" {
                    for key in ["ContainerName=", "NetworkAlias="] {
                        if let Some(name) = line.strip_prefix(key) {
                            nats_names.push(name.to_owned());
                        }
                    }
                }
            }
        }
        // Trellis reaches NATS by a name the shared network gives its container.
        let servers = url::Url::parse(nats.servers.as_deref().expect("servers")).unwrap();
        assert!(nats_names.contains(&servers.host_str().unwrap().to_owned()));
        // The CA key can issue certificates for any name; no container sees it.
        let ca_key = trellis_bootstrap::bundle_ca_directory(&out).join("ca.key");
        assert!(ca_key.is_file());
        assert!(mounted.iter().all(|source| !ca_key.starts_with(source)));
    }

    #[test]
    fn quadlet_behind_a_proxy_trusts_its_own_network_and_reaches_nats_in_plaintext() {
        let temp = tempfile::tempdir().expect("temp dir");
        let out = temp.path().join("krishi");
        let args = init_config_args(
            &out,
            &[
                "--quadlet",
                "--trusted-proxy",
                "172.31.0.0/16",
                "--trellis-origin",
                "https://krishi.example.test",
                "--nats-websocket-url",
                "wss://krishi.example.test",
                "--nats-websocket-behind-proxy",
                "--nats-websocket-only",
            ],
        );
        let mut args = args;
        args.tls = Some(super::InitTlsMode::Proxy);
        init_config(OutputFormat::Json, &args, &bundle(&out), out.clone()).expect("init");
        // The production loader accepts plaintext NATS on the private network.
        let config = trellis_runtime::RuntimeConfig::load_from_path(out.join("config.toml"))
            .expect("load production runtime config");
        let network = std::fs::read_to_string(out.join("quadlet/krishi.network")).unwrap();
        let subnet: ipnet::IpNet = network
            .lines()
            .find_map(|line| line.strip_prefix("Subnet="))
            .expect("subnet")
            .parse()
            .unwrap();
        // The network never takes the VPC's range, where the proxy is.
        let vpc: ipnet::IpNet = "172.31.0.0/16".parse().unwrap();
        assert!(!vpc.contains(&subnet.network()) && !subnet.contains(&vpc.network()));
        assert_eq!(
            config.http_transport(),
            &trellis_runtime::ListenerTransport::Proxy {
                trusted_proxies: vec![subnet]
            }
        );
        assert_eq!(
            config.resolve_nats_runtime().unwrap().servers,
            "nats://krishi-nats:4222"
        );
        assert!(
            !out.join("tls").exists(),
            "the proxy holds every certificate"
        );
    }
}
