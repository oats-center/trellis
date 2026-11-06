//! Commands that run on the host beside the server without starting it:
//! printing a config template, making a NATS operator and cluster, creating a
//! development installation, issuing test certificates, and running the `nats`
//! CLI as one of the installation's NATS users.

use std::path::{Path, PathBuf};

use miette::{miette, IntoDiagnostic as _};
use serde_json::json;
use trellis_bootstrap::{BootstrapError, NatsOperator};

/// How commands report their result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OutputFormat {
    /// Readable lines for people.
    #[default]
    Text,
    /// One JSON document for scripts.
    Json,
}

/// The commented template `trellis-server config example` prints.
pub(crate) const CONFIG_EXAMPLE: &str = include_str!("../config.example.toml");

#[derive(Debug, clap::Subcommand)]
/// Configuration files.
pub(crate) enum ConfigSubcommand {
    /// Print a commented config.toml to start from.
    Example,
}

#[derive(Debug, clap::Subcommand)]
/// Certificates.
pub(crate) enum TlsSubcommand {
    /// Show the CA fingerprint and expiry of `generated` TLS.
    Status(TlsStatusArgs),
    /// Issue a certificate for some hosts from a new single-use CA, for a
    /// lab or test installation's `tls` transport.
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
/// Report an installation's generated certificates.
pub(crate) struct TlsStatusArgs {
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

pub(crate) fn tls(command: TlsSubcommand) -> miette::Result<()> {
    match command {
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
            let (_, roots) = trellis_runtime::RuntimeConfig::load_from_path_with_defaults(
                &paths.config,
                paths.defaults(),
            )
            .into_diagnostic()?;
            let directory = roots.data.join("tls");
            let ca = trellis_runtime::tls::inspect_certificate(&directory.join("ca.crt")).ok();
            let leaf =
                trellis_runtime::tls::inspect_certificate(&directory.join("trellis.crt")).ok();
            if format == OutputFormat::Json {
                print_json(&json!({
                    "generatedTls": ca.as_ref().map(|ca| json!({
                        "caCertificate": directory.join("ca.crt").display().to_string(),
                        "caSha256Fingerprint": ca.sha256_fingerprint,
                        "caNotAfter": rfc3339(ca.not_after),
                        "leafNotAfter": leaf.as_ref().map(|leaf| rfc3339(leaf.not_after)),
                    })),
                }));
                return Ok(());
            }
            match ca {
                Some(ca) => {
                    println!("caCertificate={}", directory.join("ca.crt").display());
                    println!("caSha256Fingerprint={}", ca.sha256_fingerprint);
                    if let Some(leaf) = leaf {
                        println!("leafNotAfter={}", rfc3339(leaf.not_after));
                    }
                }
                None => println!("this installation has no Trellis-generated TLS"),
            }
            Ok(())
        }
    }
}

/// Which NATS user `trellis-server nats` connects as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NatsRole {
    System,
    Trellis,
    Auth,
}

#[derive(Debug, clap::Args)]
/// NATS: make an operator and cluster, or run the `nats` CLI as one of the
/// installation's users.
pub(crate) struct NatsArgs {
    #[command(subcommand)]
    pub(crate) command: NatsSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum NatsSubcommand {
    /// Create a NATS operator, its system account and the server configs of
    /// a new cluster, plus the two files Trellis needs from it.
    New(NatsNewArgs),
    /// Run `nats` as the system account's user: servers, accounts and
    /// connections.
    System(NatsRunArgs),
    /// Run `nats` as Trellis's user in the account services and clients run
    /// in: streams, KV and subjects.
    Trellis(NatsRunArgs),
    /// Run `nats` as the auth account's user, which answers auth callouts.
    Auth(NatsRunArgs),
}

#[derive(Debug, clap::Args)]
/// Make a NATS operator and the configs of its servers.
pub(crate) struct NatsNewArgs {
    /// Folder that receives the server configs, `system.creds` and
    /// `trellis-operator.nk`.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// Servers in the cluster. Three lets JetStream keep three copies of
    /// every stream and survive losing one server; one runs a single server
    /// without a cluster.
    #[arg(long, default_value_t = 3)]
    pub(crate) servers: u16,
    /// Address every server listens on. The servers serve plain NATS and
    /// WebSockets, so keep it private unless you add TLS to the configs.
    #[arg(long, default_value = "127.0.0.1")]
    pub(crate) listen: std::net::IpAddr,
    /// The operator's name.
    #[arg(long, default_value = "Trellis")]
    pub(crate) name: String,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, clap::Args)]
/// Run the `nats` CLI as one of the installation's NATS users.
pub(crate) struct NatsRunArgs {
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// Print the user's creds file instead of running `nats`, for a saved
    /// `nats context` or another client.
    #[arg(long, conflicts_with = "args")]
    pub(crate) print: bool,
    /// Arguments for the `nats` CLI, such as `stream ls`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

pub(crate) fn nats(args: NatsArgs) -> miette::Result<()> {
    match args.command {
        NatsSubcommand::New(args) => nats_new(&args),
        NatsSubcommand::System(args) => nats_run(NatsRole::System, args),
        NatsSubcommand::Trellis(args) => nats_run(NatsRole::Trellis, args),
        NatsSubcommand::Auth(args) => nats_run(NatsRole::Auth, args),
    }
}

/// The ports server `index` (from 0) of a `nats new` cluster listens on:
/// client, cluster routes, WebSocket and monitoring.
fn cluster_ports(index: u16) -> (u16, u16, u16, u16) {
    (4222 + index, 6222 + index, 8080 + index, 8222 + index)
}

fn nats_new(args: &NatsNewArgs) -> miette::Result<()> {
    if args.servers == 0 || args.servers > 32 {
        return Err(miette!("--servers must be between 1 and 32"));
    }
    if args.out.exists()
        && std::fs::read_dir(&args.out)
            .into_diagnostic()?
            .next()
            .is_some()
    {
        return Err(miette!("{} is not empty", args.out.display()));
    }
    std::fs::create_dir_all(&args.out).into_diagnostic()?;
    let out = std::path::absolute(&args.out).into_diagnostic()?;
    let operator = NatsOperator::new(&args.name).map_err(bootstrap_report)?;
    let host = match args.listen {
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        ip => ip.to_string(),
    };
    let mut configs = Vec::new();
    for index in 0..args.servers {
        let (client, cluster, websocket, monitor) = cluster_ports(index);
        let name = format!("nats-{}", index + 1);
        let data = out.join("data").join(&name);
        let cluster_block = if args.servers > 1 {
            let routes = (0..args.servers)
                .filter(|other| *other != index)
                .map(|other| format!("nats-route://{host}:{}", cluster_ports(other).1))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "\n# The cluster: JetStream keeps a stream's copies on different servers.\n\
                cluster {{\n  name: trellis\n  listen: {host}:{cluster}\n  routes: [{routes}]\n}}\n"
            )
        } else {
            String::new()
        };
        let config = format!(
            "# NATS server {number} of {count}, made by `trellis-server nats new`.\n\
            server_name: {name}\n\nlisten: {host}:{client}\nhttp: {host}:{monitor}\n\n\
            jetstream {{\n  store_dir: {store}\n}}\n\n\
            # Browsers and clients behind an HTTPS proxy connect here.\n\
            websocket {{\n  listen: {host}:{websocket}\n  no_tls: true\n}}\n{cluster_block}\n{operator}",
            number = index + 1,
            count = args.servers,
            store = quoted(&data),
            operator = trellis_bootstrap::render_nats_operator(
                &operator,
                &data.join("jwt").display().to_string()
            ),
        );
        let path = out.join(format!("{name}.conf"));
        std::fs::write(&path, config).into_diagnostic()?;
        configs.push(path);
    }
    let creds = out.join("system.creds");
    let signing = out.join("trellis-operator.nk");
    trellis_bootstrap::write_private_file(&creds, operator.system_user.creds())
        .map_err(bootstrap_report)?;
    trellis_bootstrap::write_private_file(&signing, format!("{}\n", operator.signing_seed))
        .map_err(bootstrap_report)?;
    let servers = (0..args.servers)
        .map(|index| format!("nats://{host}:{}", cluster_ports(index).0))
        .collect::<Vec<_>>()
        .join(",");
    if args.format == OutputFormat::Json {
        print_json(&json!({
            "configs": configs.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "systemCredentials": creds.display().to_string(),
            "operatorSigningKey": signing.display().to_string(),
            "servers": servers,
        }));
        return Ok(());
    }
    for config in &configs {
        println!("config={}", config.display());
    }
    println!("systemCredentials={}", creds.display());
    println!("operatorSigningKey={}", signing.display());
    println!();
    println!("Start each server: nats-server -c <config>. Then in Trellis's config.toml:");
    println!();
    println!("[nats]");
    println!("servers = \"{servers}\"");
    println!("system_credentials = \"{}\"", creds.display());
    println!("operator_signing_key = \"{}\"", signing.display());
    Ok(())
}

/// Runs `nats` with the installation's address, CA and the role's creds.
/// The creds exist on disk only while `nats` runs, in the installation's
/// private runtime folder.
fn nats_run(role: NatsRole, args: NatsRunArgs) -> miette::Result<()> {
    let paths = args.profile.paths(|name| std::env::var_os(name))?;
    let (config, roots) = trellis_runtime::RuntimeConfig::load_from_path_with_defaults(
        &paths.config,
        paths.defaults(),
    )
    .into_diagnostic()?;
    let nats = config
        .nats
        .as_ref()
        .ok_or_else(|| miette!("the config has no [nats]"))?;
    let users = &config.secrets.nats.users;
    let (name, creds) = match role {
        // The operator's system user lives in the file it handed over.
        NatsRole::System => (
            "system",
            match &nats.system_credentials {
                Some(path) => Some(std::fs::read_to_string(path).into_diagnostic()?),
                None => users.system.as_ref().map(trellis_runtime::NatsUser::creds),
            },
        ),
        NatsRole::Trellis => (
            "trellis",
            users.trellis.as_ref().map(trellis_runtime::NatsUser::creds),
        ),
        NatsRole::Auth => (
            "auth",
            users.auth.as_ref().map(trellis_runtime::NatsUser::creds),
        ),
    };
    let creds = creds.ok_or_else(|| {
        miette!(
            "{} has no NATS {name} user yet; start Trellis once to create it",
            trellis_runtime::SECRETS_FILE
        )
    })?;
    if args.print {
        print!("{creds}");
        return Ok(());
    }
    let servers = nats
        .servers
        .clone()
        .ok_or_else(|| miette!("the config has no [nats] servers"))?;
    crate::prepare_directory(&roots.runtime)?;
    let file = roots
        .runtime
        .join(format!("nats-{name}-{}.creds", std::process::id()));
    trellis_bootstrap::write_private_file(&file, &creds).map_err(bootstrap_report)?;
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

/// Ports of a development installation: Trellis, then its NATS server's
/// client, monitoring and WebSocket listeners.
pub(crate) const DEV_PORTS: (u16, u16, u16, u16) = (3000, 4222, 8222, 8080);

/// Creates a development installation in `folder`: a config.toml, and a NATS
/// operator for the NATS server `--dev` runs. Plain HTTP on this machine, or
/// with `host` HTTPS from a CA Trellis makes, at `https://<host>:3000`, with
/// NATS WebSockets relayed through that port.
pub(crate) fn create_dev_installation(folder: &Path, host: Option<&str>) -> miette::Result<()> {
    let (trellis, nats, monitor, websocket) = DEV_PORTS;
    crate::create_secure_directory(folder)?;
    crate::create_secure_directory(&folder.join("nats"))?;
    let operator = NatsOperator::new("Trellis development").map_err(bootstrap_report)?;
    trellis_bootstrap::write_private_file(
        folder.join("nats/system.creds"),
        operator.system_user.creds(),
    )
    .map_err(bootstrap_report)?;
    trellis_bootstrap::write_private_file(
        folder.join("nats/operator.nk"),
        format!("{}\n", operator.signing_seed),
    )
    .map_err(bootstrap_report)?;
    std::fs::write(
        folder.join("nats.conf"),
        trellis_bootstrap::render_nats_config(
            "trellis-dev",
            trellis_bootstrap::NatsListeners {
                native: nats,
                monitor,
                websocket,
            },
            None,
            trellis_bootstrap::NatsWebsocket::Relayed,
            false,
            &trellis_bootstrap::render_nats_operator(&operator, "/data/jwt"),
        ),
    )
    .into_diagnostic()?;
    let (origin, bind, transport) = match host {
        None => (
            format!("http://localhost:{trellis}"),
            "127.0.0.1",
            "mode = \"loopback_development\"",
        ),
        Some(host) => (
            format!("https://{host}:{trellis}"),
            "0.0.0.0",
            "mode = \"generated\"",
        ),
    };
    // Only this machine reaches native NATS; other machines use the
    // WebSocket the Trellis port relays.
    let clients = if host.is_some() {
        "[client]\nnats_servers = []\n"
    } else {
        ""
    };
    std::fs::write(
        folder.join("config.toml"),
        format!(
            "# A development installation made by `trellis-server --dev`. Not for\n\
            # production: Trellis runs its own NATS server here.\n\
            instance_name = \"Trellis development\"\n\n\
            [http]\npublic_origin = \"{origin}\"\nport = {trellis}\nbind_address = \"{bind}\"\n\
            nats_websocket_upstream = \"127.0.0.1:{websocket}\"\n\n\
            [http.transport]\n{transport}\n\n\
            [nats]\nmanaged = true\nservers = \"nats://127.0.0.1:{nats}\"\n\
            system_credentials = \"nats/system.creds\"\n\
            operator_signing_key = \"nats/operator.nk\"\n\n{clients}"
        ),
    )
    .into_diagnostic()
}

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap_or_default()
}

fn bootstrap_report(error: BootstrapError) -> miette::Report {
    miette::Report::new(error)
}

fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

pub(crate) fn rfc3339(value: time::OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| value.to_string())
}
