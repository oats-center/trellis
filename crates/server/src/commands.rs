//! Commands that run on the host beside the server without starting it:
//! printing a config template, making a NATS operator and cluster, creating a
//! development installation, issuing test certificates, running the `nats`
//! CLI as one of the installation's NATS users, and backing an installation up
//! and restoring it.

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
    /// The hostname of each server, such as its container's name, one per
    /// --servers. Each then listens on the same ports on its own host, and
    /// the servers route to each other by these names.
    #[arg(long, value_delimiter = ',')]
    pub(crate) hosts: Vec<String>,
    /// Client port of the first server; without --hosts each further server
    /// takes the next.
    #[arg(long, default_value_t = 4222)]
    pub(crate) client_port: u16,
    /// Cluster route port of the first server.
    #[arg(long, default_value_t = 6222)]
    pub(crate) cluster_port: u16,
    /// WebSocket port of the first server.
    #[arg(long, default_value_t = 8080)]
    pub(crate) websocket_port: u16,
    /// Monitoring port of the first server.
    #[arg(long, default_value_t = 8222)]
    pub(crate) monitor_port: u16,
    /// PEM certificate chain the servers serve native NATS and WebSockets
    /// with. Without it both are plaintext.
    #[arg(long, requires = "tls_private_key")]
    pub(crate) tls_certificate: Option<PathBuf>,
    /// Private key for --tls-certificate.
    #[arg(long, requires = "tls_certificate")]
    pub(crate) tls_private_key: Option<PathBuf>,
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
fn cluster_ports(args: &NatsNewArgs, index: u16) -> (u16, u16, u16, u16) {
    let index = if args.hosts.is_empty() { index } else { 0 };
    (
        args.client_port + index,
        args.cluster_port + index,
        args.websocket_port + index,
        args.monitor_port + index,
    )
}

fn nats_new(args: &NatsNewArgs) -> miette::Result<()> {
    if args.servers == 0 || args.servers > 32 {
        return Err(miette!("--servers must be between 1 and 32"));
    }
    if !args.hosts.is_empty() && args.hosts.len() != usize::from(args.servers) {
        return Err(miette!(
            "--hosts names {} servers, but --servers is {}",
            args.hosts.len(),
            args.servers
        ));
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
    // Where server `index` is reached: its own host, or the shared address,
    // which for every interface is this machine's loopback.
    let shared = match args.listen {
        ip if !ip.is_unspecified() => host.clone(),
        std::net::IpAddr::V4(_) => "127.0.0.1".to_owned(),
        std::net::IpAddr::V6(_) => "[::1]".to_owned(),
    };
    let reach = |index: u16| {
        args.hosts
            .get(usize::from(index))
            .cloned()
            .unwrap_or_else(|| shared.clone())
    };
    let tls = match (&args.tls_certificate, &args.tls_private_key) {
        (Some(certificate), Some(key)) => Some(format!(
            "  cert_file: {}\n  key_file: {}",
            quoted(&std::path::absolute(certificate).into_diagnostic()?),
            quoted(&std::path::absolute(key).into_diagnostic()?)
        )),
        _ => None,
    };
    let (native_tls, websocket_tls) = match &tls {
        Some(files) => (
            format!("tls {{\n{files}\n}}\n\n"),
            format!("  tls {{\n  {}\n  }}", files.replace('\n', "\n  ")),
        ),
        None => (String::new(), "  no_tls: true".to_owned()),
    };
    let mut configs = Vec::new();
    for index in 0..args.servers {
        let (client, cluster, websocket, monitor) = cluster_ports(args, index);
        let name = format!("nats-{}", index + 1);
        let data = out.join("data").join(&name);
        let cluster_block = if args.servers > 1 {
            let routes = (0..args.servers)
                .filter(|other| *other != index)
                .map(|other| {
                    format!(
                        "nats-route://{}:{}",
                        reach(other),
                        cluster_ports(args, other).1
                    )
                })
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
            {native_tls}jetstream {{\n  store_dir: {store}\n}}\n\n\
            # Browsers and clients connect here, or a proxy in front of it.\n\
            websocket {{\n  listen: {host}:{websocket}\n{websocket_tls}\n}}\n{cluster_block}\n{operator}",
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
    let scheme = if tls.is_some() { "tls" } else { "nats" };
    let servers = (0..args.servers)
        .map(|index| {
            format!(
                "{scheme}://{}:{}",
                reach(index),
                cluster_ports(args, index).0
            )
        })
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

#[derive(Debug, clap::Args)]
/// Save a recovery point of an installation.
pub(crate) struct BackupArgs {
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// New or empty folder that receives the backup.
    pub(crate) destination: PathBuf,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, clap::Args)]
/// Put a backup into an installation folder.
pub(crate) struct RestoreArgs {
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// Folder a `trellis-server backup` wrote.
    pub(crate) source: PathBuf,
    /// Replace an installation with another ID or a `config.toml` that
    /// can't be loaded, and files outside the installation folder that exist
    /// with other contents.
    #[arg(long)]
    pub(crate) force: bool,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, clap::Args)]
/// Write the installation's configuration into a passphrase-sealed file.
pub(crate) struct ExportArgs {
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// The file to write. An existing file is never replaced.
    #[arg(long, value_name = "FILE")]
    pub(crate) out: PathBuf,
    /// Read the passphrase from this file, or `-` for standard input.
    /// Without it, piped standard input is read.
    #[arg(long, value_name = "PATH")]
    pub(crate) passphrase_file: Option<PathBuf>,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

#[derive(Debug, clap::Args)]
/// Add an installation export to this installation.
pub(crate) struct ImportArgs {
    #[command(flatten)]
    pub(crate) profile: crate::InstallationArgs,
    /// A file `trellis-server export` wrote.
    pub(crate) file: PathBuf,
    /// Read the passphrase from this file, or `-` for standard input.
    /// Without it, piped standard input is read.
    #[arg(long, value_name = "PATH")]
    pub(crate) passphrase_file: Option<PathBuf>,
    /// Report what the import would create and what is in the way,
    /// changing nothing.
    #[arg(long)]
    pub(crate) check: bool,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

/// The configuration of an installation that has started once, with the
/// installation ID its first start kept in `secrets.toml`.
fn installation_config(
    profile: &crate::InstallationArgs,
) -> miette::Result<trellis_runtime::RuntimeConfig> {
    let paths = profile.paths(|name| std::env::var_os(name))?;
    let (mut config, _) = trellis_runtime::RuntimeConfig::load_from_path_with_defaults(
        &paths.config,
        paths.defaults(),
    )
    .into_diagnostic()?;
    if let (Some(auth), Some(instance)) = (config.auth.as_mut(), config.secrets.instance_id.clone())
    {
        auth.instance_id.get_or_insert(instance);
    }
    Ok(config)
}

/// The passphrase from `file` (`-` for standard input), else from piped
/// standard input. A new file's passphrase has at least the envelope's
/// minimum length.
fn passphrase(file: Option<&Path>, new: bool) -> miette::Result<String> {
    use std::io::IsTerminal as _;
    let text = match file {
        Some(path) if path != Path::new("-") => std::fs::read_to_string(path)
            .map_err(|error| miette!("cannot read {}: {error}", path.display()))?,
        None if std::io::stdin().is_terminal() => {
            return Err(miette!(
                "give the passphrase with --passphrase-file PATH, or `-` to read it from \
                standard input"
            ));
        }
        _ => std::io::read_to_string(std::io::stdin()).into_diagnostic()?,
    };
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    if text.is_empty() {
        return Err(miette!("the passphrase is empty"));
    }
    let minimum = trellis_bootstrap::envelope::MIN_PASSPHRASE;
    if new && text.chars().count() < minimum {
        return Err(miette!(
            "an installation export's passphrase needs at least {minimum} characters"
        ));
    }
    Ok(text)
}

/// Writes the installation's configuration into `--out`, sealed with a
/// passphrase, through a temporary file beside it that becomes `--out` only
/// once whole, and never in place of an existing file.
pub(crate) async fn export(args: ExportArgs) -> miette::Result<()> {
    use std::io::Write as _;
    let passphrase = passphrase(args.passphrase_file.as_deref(), true)?;
    let config = installation_config(&args.profile)?;
    let out = std::path::absolute(&args.out).into_diagnostic()?;
    if out.exists() {
        return Err(miette!("{} exists; choose another file", out.display()));
    }
    let partial = out.with_file_name(format!(
        ".{}.{}.partial",
        out.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("export"),
        std::process::id()
    ));
    let file = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&partial)
            .map_err(|error| miette!("cannot write {}: {error}", partial.display()))?
    };
    let result = async {
        let mut sealed = trellis_bootstrap::envelope::Encryptor::new(
            std::io::BufWriter::new(file),
            trellis_bootstrap::envelope::INSTALLATION,
            &passphrase,
        )?;
        let report = {
            let mut writer = SealedWriter(&mut sealed);
            trellis_runtime::platform::export_installation(&config, &mut writer)
                .await
                .into_diagnostic()?
        };
        sealed.finish()?.flush().into_diagnostic()?;
        // A hard link refuses an existing name, so a file that appeared
        // meanwhile stays as it was.
        std::fs::hard_link(&partial, &out)
            .map_err(|error| miette!("cannot create {}: {error}", out.display()))?;
        Ok::<_, miette::Report>(report)
    }
    .await;
    let _ = std::fs::remove_file(&partial);
    let report = result?;
    if args.format == OutputFormat::Json {
        print_json(&json!({ "file": out.display().to_string(), "report": report }));
        return Ok(());
    }
    println!(
        "exported installation {} into {}",
        report["sourceInstallationId"]
            .as_str()
            .unwrap_or("without an ID"),
        out.display()
    );
    if let Some(records) = report["records"].as_object() {
        for (kind, count) in records {
            println!("  {kind}: {count}");
        }
    }
    for deployment in report["deployments"].as_array().into_iter().flatten() {
        println!(
            "  deployment {} ({})",
            deployment["title"].as_str().unwrap_or_default(),
            deployment["deploymentId"].as_str().unwrap_or_default()
        );
    }
    println!("Not carried:");
    for item in report["notCarried"].as_array().into_iter().flatten() {
        println!("  {}", item.as_str().unwrap_or_default());
    }
    Ok(())
}

/// `Write` for the content an export streams into the envelope.
struct SealedWriter<'a, W: std::io::Write>(&'a mut trellis_bootstrap::envelope::Encryptor<W>);

impl<W: std::io::Write> std::io::Write for SealedWriter<'_, W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .write(data)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Adds an installation export to a stopped installation, or with
/// `--check` reports what it would do.
pub(crate) async fn import(args: ImportArgs) -> miette::Result<()> {
    let passphrase = passphrase(args.passphrase_file.as_deref(), false)?;
    let config = installation_config(&args.profile)?;
    let file = std::fs::File::open(&args.file)
        .map_err(|error| miette!("cannot read {}: {error}", args.file.display()))?;
    let mut opened = trellis_bootstrap::envelope::Decryptor::open(
        std::io::BufReader::new(file),
        trellis_bootstrap::envelope::INSTALLATION,
        &passphrase,
    )?;
    let report = trellis_runtime::platform::import_installation(&config, &mut opened, args.check)
        .await
        .into_diagnostic()?;
    if args.format == OutputFormat::Json {
        print_json(&json!({ "import": report }));
        return Ok(());
    }
    println!(
        "{} installation {} from {}",
        if args.check { "checked" } else { "imported" },
        report["source"]["installationId"]
            .as_str()
            .unwrap_or_default(),
        report["source"]["origin"].as_str().unwrap_or_default()
    );
    if let Some(existing) = report["target"]["existing"]
        .as_object()
        .filter(|found| !found.is_empty())
    {
        println!("This installation is not empty; it already has:");
        for (kind, count) in existing {
            println!("  {kind}: {count}");
        }
    }
    let line = |item: &serde_json::Value| {
        let mut line = format!(
            "  {} {}",
            item["kind"].as_str().unwrap_or_default(),
            item["id"]
                .as_str()
                .or(item["roleId"].as_str())
                .or(item["capability"].as_str())
                .unwrap_or_default()
        );
        if let Some(reason) = item["reason"].as_str() {
            line.push_str(&format!(": {reason}"));
        }
        line
    };
    println!("Created:");
    for item in report["created"].as_array().into_iter().flatten() {
        println!("{}", line(item));
    }
    for item in report["access"]["recreated"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!("{}", line(item));
    }
    println!("Found here as the same:");
    for item in report["matched"].as_array().into_iter().flatten() {
        println!("{}", line(item));
    }
    println!("Settings replaced with the export's:");
    for item in report["changed"].as_array().into_iter().flatten() {
        println!(
            "{}\n    was {}\n    now {}",
            line(item),
            item["old"],
            item["new"]
        );
    }
    if let Some(detail) = report["administrator"]["detail"].as_str() {
        println!("Administrator: {detail}");
    }
    println!("Skipped:");
    for item in report["skipped"].as_array().into_iter().flatten() {
        println!("{}", line(item));
    }
    for item in report["access"]["skipped"].as_array().into_iter().flatten() {
        println!("{}", line(item));
    }
    print!("{}", pending_text(&report));
    println!("Not carried:");
    for item in report["notCarried"].as_array().into_iter().flatten() {
        println!("  {}", item.as_str().unwrap_or_default());
    }
    let unread = report["notUnderstood"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !unread.is_empty() {
        println!("Not understood, left out:");
        for item in unread {
            println!("  {}", item.as_str().unwrap_or_default());
        }
    }
    Ok(())
}

/// What an import report says is not finished yet, as text: each item with
/// what finishes it, and how to go on. Empty when the import finished.
fn pending_text(report: &serde_json::Value) -> String {
    let pending = report["pending"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if pending.is_empty() {
        return String::new();
    }
    let mut text = String::from("Not finished yet:\n");
    for item in pending {
        let what = match item["kind"].as_str().unwrap_or_default() {
            "deploymentData" => "data of deployment",
            "deviceState" => "device State of",
            kind => kind,
        };
        text.push_str(&format!(
            "  {what} {}: {}\n",
            item["id"].as_str().unwrap_or_default(),
            item["reason"].as_str().unwrap_or_default()
        ));
    }
    let applied = report["applied"] == true;
    text.push_str(if applied {
        "Run the same import again once the steps above are done; what is already here is \
        kept and the rest is finished.\n"
    } else {
        "The import brings these once the steps above are done and it is run again.\n"
    });
    text
}

/// Backs up an installation, running or stopped, while its NATS runs.
pub(crate) async fn backup(args: BackupArgs) -> miette::Result<()> {
    let paths = args.profile.paths(|name| std::env::var_os(name))?;
    let (config, roots) = trellis_runtime::RuntimeConfig::load_from_path_with_defaults(
        &paths.config,
        paths.defaults(),
    )
    .into_diagnostic()?;
    let destination = std::path::absolute(&args.destination).into_diagnostic()?;
    let manifest =
        trellis_runtime::backup::backup(&config, &paths.config, &roots.data, &destination)
            .await
            .into_diagnostic()?;
    let streams = manifest.streams().count();
    if args.format == OutputFormat::Json {
        print_json(&json!({
            "backup": destination.display().to_string(),
            "instanceId": manifest.instance_id,
            "createdAt": manifest.created_at,
            "files": manifest.entries.len(),
            "streams": streams,
        }));
        return Ok(());
    }
    println!(
        "backed up installation {} into {}: {} files, {streams} of them NATS streams",
        manifest.instance_id.as_deref().unwrap_or("without an ID"),
        destination.display(),
        manifest.entries.len()
    );
    Ok(())
}

/// Restores a backup into a stopped or fresh installation folder.
pub(crate) fn restore(args: RestoreArgs) -> miette::Result<()> {
    let paths = args.profile.paths(|name| std::env::var_os(name))?;
    let manifest =
        trellis_runtime::backup::restore(&args.source, &paths.config, paths.defaults(), args.force)
            .into_diagnostic()?;
    let streams = manifest.streams().count();
    if args.format == OutputFormat::Json {
        print_json(&json!({
            "config": paths.config.display().to_string(),
            "instanceId": manifest.instance_id,
            "createdAt": manifest.created_at,
            "stagedStreams": streams,
        }));
        return Ok(());
    }
    println!(
        "restored installation {} as it was at {} into {}",
        manifest.instance_id.as_deref().unwrap_or("without an ID"),
        manifest.created_at,
        paths.config.display()
    );
    println!(
        "Start it against NATS with an empty JetStream store; the first start loads its \
        {streams} streams."
    );
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// The text an operator reads after an import names each deployment
    /// whose data has not moved yet, why, and that running the same import
    /// again finishes it; an import that finished prints nothing of that.
    #[test]
    fn import_text_names_what_is_not_finished_and_how_to_finish_it() {
        let reason = "the deployment has no instance that can run here yet";
        let report = json!({
            "applied": true,
            "pending": [{
                "kind": "deploymentData",
                "id": "01J0000000000000000000000A",
                "reason": reason
            }]
        });
        let text = super::pending_text(&report);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Not finished yet:");
        assert_eq!(
            lines[1],
            format!("  data of deployment 01J0000000000000000000000A: {reason}")
        );
        assert!(lines[2].starts_with("Run the same import again"));
        assert_eq!(
            super::pending_text(&json!({ "applied": true, "pending": [] })),
            ""
        );
    }
}
