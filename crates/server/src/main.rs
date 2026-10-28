//! Command-line entrypoint and host policy for the Trellis server process.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use miette::{miette, IntoDiagnostic as _};
use tracing::{error, info};
use trellis_local_nats::{
    LocalNats, LocalNatsPorts, NatsBinarySource, NatsOutput, NatsServerBinary,
};
use trellis_runtime::{
    NatsEndpointOverride, RuntimeConfig, RuntimeMode, RuntimeOptions, RuntimePathDefaults,
};

mod setup;
mod telemetry;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Run the Trellis server",
    args_conflicts_with_subcommands = true
)]
struct Args {
    #[command(subcommand)]
    operation: Option<OperationArgs>,
    #[command(flatten)]
    server: ServerArgs,
    /// Starting administrator username; defaults to admin on a fresh installation.
    #[arg(long)]
    admin_username: Option<String>,
    /// Starting administrator password. Omit to receive a one-time setup link.
    #[arg(long, conflicts_with = "admin_password_stdin")]
    admin_password: Option<String>,
    /// Read the starting administrator password from standard input.
    #[arg(long)]
    admin_password_stdin: bool,
    /// Create a localhost installation in the default location without asking
    /// when there is none yet.
    #[arg(long)]
    init: bool,
    /// Issue a one-time password-reset URL for the sole active administrator.
    #[arg(long)]
    reset_admin: bool,
    /// Runtime mode to run: all, platform, jobs, health, or events.
    #[arg(default_value = "all")]
    mode: RuntimeMode,
    /// How a first-run installation reports its result.
    #[arg(long, value_enum, default_value_t)]
    format: setup::OutputFormat,
}

#[derive(Debug, Subcommand)]
enum OperationArgs {
    /// Validate configuration and runtime dependencies, then exit.
    Check(CheckArgs),
    /// Set up this host: generate its configuration bundle, or with `admin`
    /// seed the first administrator.
    Init(Box<InitArgs>),
    /// Manage Trellis-generated TLS certificates.
    #[command(subcommand)]
    Tls(setup::TlsSubcommand),
    /// Replace this binary with the newest trellis-server release for this
    /// platform.
    Upgrade(UpgradeArgs),
}

#[derive(Debug, clap::Args)]
struct UpgradeArgs {
    /// Only report whether a newer release exists.
    #[arg(long)]
    check: bool,
    /// Include prerelease versions such as release candidates.
    #[arg(long)]
    prerelease: bool,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    format: setup::OutputFormat,
}

#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct InitArgs {
    #[command(subcommand)]
    admin: Option<InitSubcommand>,
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    config: setup::InitConfigArgs,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    format: setup::OutputFormat,
}

#[derive(Debug, Subcommand)]
enum InitSubcommand {
    /// Seed the first local administrator in the configured storage, then exit.
    Admin(InitAdminArgs),
}

#[derive(Debug, clap::Args)]
struct InitAdminArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    bootstrap: BootstrapAdminArgs,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    format: setup::OutputFormat,
}

#[derive(Debug, clap::Args)]
struct CheckArgs {
    #[command(flatten)]
    server: ServerArgs,
    /// Runtime mode to validate: all, platform, jobs, health, or events.
    #[arg(default_value = "all")]
    mode: RuntimeMode,
}

/// Initialize the first local administrator with a password or a one-time setup link.
#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
struct BootstrapAdminArgs {
    /// Login username for the first administrator.
    #[arg(long, default_value = "admin")]
    username: String,
    /// Password in argv. Prefer --password-stdin outside interactive shells.
    #[arg(long, conflicts_with = "password_stdin")]
    password: Option<String>,
    /// Read the password from standard input.
    #[arg(long)]
    password_stdin: bool,
}

/// Which installation to use.
#[derive(Debug, Default, clap::Args)]
pub(crate) struct ProfileArgs {
    /// Override the profile's config.toml.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Use Linux system/FHS paths instead of the default user profile.
    #[arg(long)]
    pub(crate) system: bool,
}

impl ProfileArgs {
    fn paths(
        self,
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> miette::Result<ServerPaths> {
        if self.system {
            ServerPaths::system(self.config)
        } else {
            ServerPaths::user(self.config, get)
        }
    }
}

/// Options shared by starting and checking a server.
#[derive(Debug, Default, clap::Args)]
struct ServerArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    /// Run this exact nats-server executable instead of the one on PATH, for an
    /// installation whose NATS this server runs.
    #[arg(long, value_name = "PATH", require_equals = true)]
    local_nats: Option<PathBuf>,
    /// Download the pinned, checksum-verified NATS release without asking, for
    /// an installation whose NATS this server runs.
    #[arg(long)]
    nats_download: bool,
    /// Managed NATS client, monitor, and websocket ports (default: 4222,8222,8080).
    #[arg(long, num_args = 1, value_delimiter = ',')]
    local_nats_ports: Option<Vec<u16>>,
    /// Shared verified managed-NATS executable cache; requires managed NATS.
    #[arg(long)]
    local_nats_cache: Option<PathBuf>,
    /// Create a dedicated Unix process group so a supervisor can terminate this host and its
    /// children together.
    #[arg(long)]
    isolated_process_group: bool,
    /// Native NATS endpoint advertised to clients only; repeatable.
    #[arg(long)]
    advertise_nats_server: Vec<String>,
    /// WebSocket NATS endpoint advertised to clients only; repeatable.
    #[arg(long)]
    advertise_nats_websocket: Vec<String>,
    /// Show Trellis debug logs and copy managed NATS output to the terminal.
    /// Libraries stay at info unless RUST_LOG says otherwise.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Run,
    Check,
    BootstrapAdmin,
}

/// Which nats-server runs when the installation's config says this server
/// runs NATS.
#[derive(Clone, Debug, Eq, PartialEq)]
enum NatsBinary {
    /// `nats-server` from PATH, or the pinned download once the operator agrees.
    Automatic,
    Path(PathBuf),
    Download,
}

#[derive(Debug, Eq, PartialEq)]
struct StartupPolicy {
    admin_username: Option<String>,
    admin_password: Option<String>,
    admin_password_stdin: bool,
    operation: Operation,
    mode: RuntimeMode,
    paths: ServerPaths,
    system: bool,
    config_given: bool,
    init_missing: bool,
    nats_binary: NatsBinary,
    nats_ports: Option<LocalNatsPorts>,
    nats_cache: Option<PathBuf>,
    advertised_native: Option<Vec<String>>,
    advertised_websocket: Option<Vec<String>>,
    verbose: bool,
    reset_admin: bool,
    bootstrap_admin: Option<BootstrapAdminArgs>,
    format: setup::OutputFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ServerPaths {
    config_root: PathBuf,
    data_root: PathBuf,
    state_root: PathBuf,
    cache_root: PathBuf,
    runtime_root: PathBuf,
    log_root: PathBuf,
    config: PathBuf,
    runtime_fallback_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedNatsPaths {
    source: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    pid: PathBuf,
    log: PathBuf,
}

impl StartupPolicy {
    fn resolve(args: Args) -> miette::Result<Self> {
        Self::resolve_with(args, |name| env::var_os(name))
    }

    fn resolve_with(
        args: Args,
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> miette::Result<Self> {
        let (operation, server, mode, bootstrap_admin, format) = match args.operation {
            Some(OperationArgs::Check(check)) => (
                Operation::Check,
                check.server,
                check.mode,
                None,
                args.format,
            ),
            Some(OperationArgs::Init(init)) => {
                let Some(InitSubcommand::Admin(admin)) = init.admin else {
                    return Err(miette!("trellis-server init runs without a server policy"));
                };
                (
                    Operation::BootstrapAdmin,
                    ServerArgs {
                        profile: admin.profile,
                        ..ServerArgs::default()
                    },
                    args.mode,
                    Some(admin.bootstrap),
                    admin.format,
                )
            }
            Some(OperationArgs::Tls(_) | OperationArgs::Upgrade(_)) => {
                return Err(miette!(
                    "this trellis-server command runs without a server policy"
                ));
            }
            None => (Operation::Run, args.server, args.mode, None, args.format),
        };
        let system = server.profile.system;
        let config_given = server.profile.config.is_some();
        let paths = server.profile.paths(get)?;
        let nats_binary = match (server.local_nats, server.nats_download) {
            (Some(_), true) => {
                return Err(miette!(
                    "--local-nats=<PATH> conflicts with --nats-download"
                ));
            }
            (Some(path), false) => NatsBinary::Path(path),
            (None, true) => NatsBinary::Download,
            (None, false) => NatsBinary::Automatic,
        };
        for (endpoints, schemes) in [
            (&server.advertise_nats_server, &["nats", "tls"][..]),
            (&server.advertise_nats_websocket, &["ws", "wss"][..]),
        ] {
            for endpoint in endpoints {
                let url = url::Url::parse(endpoint)
                    .map_err(|_| miette!("invalid advertised NATS endpoint"))?;
                if !schemes.contains(&url.scheme())
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(miette!("invalid advertised NATS endpoint"));
                }
            }
        }
        let nats_ports = match server.local_nats_ports {
            Some(ports) => {
                let [nats, monitor, websocket]: [u16; 3] = ports
                    .try_into()
                    .map_err(|_| miette!("--local-nats-ports requires three ports"))?;
                if nats == 0
                    || monitor == 0
                    || websocket == 0
                    || nats == monitor
                    || nats == websocket
                    || monitor == websocket
                {
                    return Err(miette!(
                        "--local-nats-ports requires three distinct nonzero ports"
                    ));
                }
                Some(LocalNatsPorts {
                    nats,
                    monitor,
                    websocket,
                })
            }
            None => None,
        };
        Ok(Self {
            admin_username: args.admin_username,
            admin_password: args.admin_password,
            admin_password_stdin: args.admin_password_stdin,
            operation,
            mode,
            paths,
            system,
            config_given,
            init_missing: args.init,
            nats_binary,
            nats_ports,
            nats_cache: server.local_nats_cache,
            advertised_native: (!server.advertise_nats_server.is_empty())
                .then_some(server.advertise_nats_server),
            advertised_websocket: (!server.advertise_nats_websocket.is_empty())
                .then_some(server.advertise_nats_websocket),
            verbose: server.verbose,
            reset_admin: args.reset_admin,
            bootstrap_admin,
            format,
        })
    }
}

fn load_runtime_config(
    policy: &StartupPolicy,
) -> miette::Result<(RuntimeConfig, RuntimePathDefaults)> {
    RuntimeConfig::load_from_path_with_defaults(
        &policy.paths.config,
        RuntimePathDefaults {
            data: policy.paths.data_root.clone(),
            state: policy.paths.state_root.clone(),
            cache: policy.paths.cache_root.clone(),
            runtime: policy.paths.runtime_root.clone(),
            logs: policy.paths.log_root.clone(),
        },
    )
    .into_diagnostic()
}

impl ServerPaths {
    fn system(config: Option<PathBuf>) -> miette::Result<Self> {
        let defaults = RuntimePathDefaults::system_profile().into_diagnostic()?;
        let config_root = PathBuf::from("/etc/trellis");
        Ok(Self::new(
            config_root.clone(),
            defaults.data,
            defaults.state,
            defaults.cache,
            defaults.runtime,
            defaults.logs,
            config.unwrap_or_else(|| config_root.join("config.toml")),
        ))
    }

    fn user(
        config: Option<PathBuf>,
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> miette::Result<Self> {
        let home = get("HOME")
            .or_else(|| get("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| miette!("cannot resolve user profile: HOME is not set"))?;
        let root = |variable: &str, fallback: &str| {
            get(variable)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(fallback))
                .join("trellis")
        };
        let config_root = root("XDG_CONFIG_HOME", ".config");
        let defaults = RuntimePathDefaults::user_profile(&get).into_diagnostic()?;
        let runtime_fallback_root = if get("XDG_RUNTIME_DIR").is_none() {
            defaults.runtime.parent().map(Path::to_path_buf)
        } else {
            None
        };
        let mut paths = Self::new(
            config_root.clone(),
            defaults.data,
            defaults.state,
            defaults.cache,
            defaults.runtime,
            defaults.logs,
            config.unwrap_or_else(|| config_root.join("config.toml")),
        );
        paths.runtime_fallback_root = runtime_fallback_root;
        Ok(paths)
    }

    fn new(
        config_root: PathBuf,
        data_root: PathBuf,
        state_root: PathBuf,
        cache_root: PathBuf,
        runtime_root: PathBuf,
        log_root: PathBuf,
        config: PathBuf,
    ) -> Self {
        let config_root = config.parent().unwrap_or(&config_root).to_path_buf();
        Self {
            config_root,
            data_root,
            state_root,
            cache_root,
            runtime_root,
            log_root,
            config,
            runtime_fallback_root: None,
        }
    }
}

impl ManagedNatsPaths {
    fn resolve(config_root: &Path, paths: &RuntimePathDefaults) -> Self {
        Self {
            source: config_root.join("nats"),
            state: paths.state.join("nats"),
            cache: paths.cache.join("nats"),
            pid: paths.runtime.join("nats-server.pid"),
            log: paths.logs.join("nats-server.log"),
        }
    }
}

fn create_secure_directory(path: &Path) -> miette::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path).into_diagnostic()
}

fn prepare_directory(path: &Path) -> miette::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(miette!("{} is not a directory", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_secure_directory(path)?;
            let metadata = fs::symlink_metadata(path).into_diagnostic()?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(miette!("{} is not a directory", path.display()))
            }
        }
        Err(error) => Err(error).into_diagnostic(),
    }
}

#[cfg(unix)]
fn ensure_private_runtime_fallback(path: &Path) -> miette::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let user = unsafe { libc::geteuid() };
    let parent = path
        .parent()
        .ok_or_else(|| miette!("runtime fallback has no parent: {}", path.display()))?;
    let parent_metadata = fs::symlink_metadata(parent).into_diagnostic()?;
    let parent_mode = parent_metadata.mode();
    let safe_parent = parent_metadata.file_type().is_dir()
        && !parent_metadata.file_type().is_symlink()
        && ((parent_metadata.uid() == 0 && parent_mode & 0o1000 != 0)
            || (parent_metadata.uid() == user && parent_mode & 0o022 == 0));
    if !safe_parent {
        return Err(miette!(
            "runtime fallback parent is unsafe: {}",
            parent.display()
        ));
    }

    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_secure_directory(path)?;
        }
        Err(error) => return Err(error).into_diagnostic(),
    }
    let metadata = fs::symlink_metadata(path).into_diagnostic()?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_dir()
        || metadata.uid() != user
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(miette!(
            "runtime fallback must be a user-owned 0700 directory: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_runtime_fallback(path: &Path) -> miette::Result<()> {
    prepare_directory(path)
}

async fn run(policy: StartupPolicy) -> miette::Result<()> {
    if !policy.paths.config.is_file() {
        // Only a plain start in the user profile offers a first installation;
        // an explicit --config or a --system host names one that must exist.
        if policy.config_given || policy.system || policy.operation != Operation::Run {
            return Err(miette!(
                "Trellis configuration file not found at {}",
                policy.paths.config.display()
            ));
        }
        let folder = &policy.paths.config_root;
        let create = policy.init_missing
            || confirm(&format!(
                "There is no Trellis installation in {} yet. Create one for this machine (localhost)?",
                folder.display()
            ))?;
        if !create {
            return Err(miette!(
                "no Trellis installation in {}; create one with `trellis-server init` \
                (it takes hostnames for other machines), pass --init to create a \
                localhost one without asking, or name one with --config",
                folder.display()
            ));
        }
        #[derive(Parser)]
        struct DefaultInit {
            #[command(flatten)]
            args: setup::InitConfigArgs,
        }
        let defaults = DefaultInit::parse_from(["init"]).args;
        setup::init_config(policy.format, &defaults, false, folder.clone())?;
    }
    let (mut config, paths) = load_runtime_config(&policy)?;

    if let Some(bootstrap) = policy.bootstrap_admin.as_ref() {
        let password =
            resolve_bootstrap_password(bootstrap.password.as_deref(), bootstrap.password_stdin)?;
        let (principal, account_url) = trellis_runtime::platform::initialize_admin_credentials(
            &config,
            &bootstrap.username,
            password.as_deref(),
        )
        .await
        .into_diagnostic()?;
        if policy.format == setup::OutputFormat::Json {
            println!(
                "{}",
                serde_json::json!({"principalId":principal,"adminAccountUrl":account_url})
            );
        } else {
            println!("initialized administrator '{principal}'");
            if let Some(url) = account_url {
                println!("adminAccountUrl={url}");
            }
        }
        return Ok(());
    }
    if policy.admin_username.is_some()
        || policy.admin_password.is_some()
        || policy.admin_password_stdin
    {
        if policy.operation != Operation::Run
            || policy.reset_admin
            || !matches!(policy.mode, RuntimeMode::All | RuntimeMode::Platform)
        {
            return Err(miette!(
                "initial administrator options require a fresh platform startup and \
                cannot be combined with --reset-admin"
            ));
        }
        let password = resolve_bootstrap_password(
            policy.admin_password.as_deref(),
            policy.admin_password_stdin,
        )?;
        let (_, account_url) = trellis_runtime::platform::initialize_admin_credentials(
            &config,
            policy.admin_username.as_deref().unwrap_or("admin"),
            password.as_deref(),
        )
        .await
        .into_diagnostic()?;
        if let Some(url) = account_url {
            println!("{}", serde_json::json!({"adminAccountUrl":url}));
        }
    }
    if let Some(root) = &policy.paths.runtime_fallback_root {
        if paths.runtime.starts_with(root) {
            ensure_private_runtime_fallback(root)?;
        }
    }
    tracing::debug!(
        config_root = %policy.paths.config_root.display(),
        data_root = %paths.data.display(),
        state_root = %paths.state.display(),
        cache_root = %paths.cache.display(),
        runtime_root = %paths.runtime.display(),
        log_root = %paths.logs.display(),
        "resolved server paths"
    );
    if policy.operation == Operation::Run {
        prepare_directory(&paths.data)?;
        prepare_directory(&paths.state)?;
        prepare_directory(&paths.runtime)?;
        prepare_directory(&paths.logs)?;
    }
    // A start reports a broken config before it fetches or starts NATS; check
    // reports one in its JSON report instead.
    if policy.operation == Operation::Run {
        config.validate_for_mode(policy.mode).into_diagnostic()?;
    }
    let mut managed = None;
    let managed_nats = config.nats.as_ref().is_some_and(|nats| nats.managed);
    if !managed_nats {
        for (given, flag) in [
            (
                policy.nats_binary != NatsBinary::Automatic,
                "--local-nats or --nats-download",
            ),
            (policy.nats_ports.is_some(), "--local-nats-ports"),
            (policy.nats_cache.is_some(), "--local-nats-cache"),
        ] {
            if given {
                return Err(miette!(
                    "{flag} only applies when trellis-server runs NATS, but {} connects \
                    to the NATS servers in [nats] servers",
                    policy.paths.config.display()
                ));
            }
        }
    }
    let nats_override = match managed_nats {
        false => {
            info!(config = %policy.paths.config.display(), "using configured external NATS");
            if policy.advertised_native.is_some() || policy.advertised_websocket.is_some() {
                Some(NatsEndpointOverride {
                    runtime_servers: config.resolve_nats_runtime().into_diagnostic()?.servers,
                    advertised_native: policy.advertised_native.clone(),
                    advertised_websocket: policy.advertised_websocket.clone(),
                })
            } else {
                None
            }
        }
        true => {
            let mut managed_paths = ManagedNatsPaths::resolve(&policy.paths.config_root, &paths);
            if let Some(cache) = &policy.nats_cache {
                managed_paths.cache = cache.clone();
            }
            let source = match &policy.nats_binary {
                NatsBinary::Path(path) => NatsBinarySource::Path(path.clone()),
                NatsBinary::Download => NatsBinarySource::DownloadPinned,
                NatsBinary::Automatic if NatsServerBinary::from_path_lookup().is_ok() => {
                    NatsBinarySource::PathLookup
                }
                NatsBinary::Automatic => {
                    let version = trellis_local_nats::pinned_version().into_diagnostic()?;
                    let download = confirm(&format!(
                        "nats-server is not on your PATH. Download NATS {version} \
                        (checksum-verified) into {}?",
                        managed_paths.cache.display()
                    ))?;
                    if !download {
                        return Err(miette!(
                            "nats-server is not on PATH; install it, pass --nats-download \
                            to download the pinned release, or name one with \
                            --local-nats=<PATH>"
                        ));
                    }
                    NatsBinarySource::DownloadPinned
                }
            };
            prepare_directory(&managed_paths.state)?;
            prepare_directory(&paths.runtime)?;
            prepare_directory(&paths.logs)?;
            if matches!(source, NatsBinarySource::DownloadPinned) {
                prepare_directory(&managed_paths.cache)?;
            }
            info!(log = %managed_paths.log.display(), "starting managed NATS");
            // Managed listeners follow the authored bundle's nats.conf unless an
            // operator passes an explicit --local-nats-ports override. There is no
            // default substitution at startup: a bundle without explicit listeners
            // is a configuration error, never a silent fallback to 4222/8222/8080.
            let nats_ports = match policy.nats_ports {
                Some(ports) => ports,
                None => {
                    let nats_config = managed_paths.source.join("nats.conf");
                    let listeners = trellis_bootstrap::read_nats_listen_ports(&nats_config)
                        .into_diagnostic()?;
                    LocalNatsPorts {
                        nats: listeners.native,
                        monitor: listeners.monitor,
                        websocket: listeners.websocket,
                    }
                }
            };
            let server = LocalNats::builder()
                .binary(source)
                .source(managed_paths.source)
                .state(managed_paths.state)
                .ports(nats_ports)
                .cache_dir(managed_paths.cache)
                .pid_file(managed_paths.pid)
                .output(NatsOutput::Log {
                    path: managed_paths.log,
                    mirror: policy.verbose,
                })
                .start()
                .into_diagnostic()?;
            info!("managed NATS ready");
            let servers = server.nats_url().to_string();
            // A runtime relaying browser WebSockets relays to the broker it
            // just started, and keeps advertising its own origin.
            let websocket = match config.http.as_mut() {
                Some(http) if http.nats_websocket_upstream.is_some() => {
                    http.nats_websocket_upstream =
                        Some(format!("127.0.0.1:{}", nats_ports.websocket));
                    config.same_origin_websocket_url()
                }
                _ => Some(server.websocket_url().to_string()),
            };
            managed = Some(server);
            Some(NatsEndpointOverride {
                runtime_servers: servers.clone(),
                advertised_native: policy
                    .advertised_native
                    .clone()
                    .or_else(|| Some(vec![servers])),
                advertised_websocket: policy
                    .advertised_websocket
                    .clone()
                    .or_else(|| websocket.map(|url| vec![url])),
            })
        }
    };

    let result = if policy.operation == Operation::Check {
        let report = trellis_runtime::check(
            policy.mode,
            policy.paths.config.clone(),
            config,
            nats_override
                .as_ref()
                .map(|value| value.runtime_servers.as_str()),
        )
        .await
        .into_diagnostic()?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report).into_diagnostic()?
        );
        if report.valid {
            Ok(())
        } else {
            Err(miette!("runtime preflight checks failed"))
        }
    } else {
        trellis_runtime::run(RuntimeOptions {
            mode: policy.mode,
            config,
            reset_admin: policy.reset_admin,
            nats_override,
        })
        .await
        .into_diagnostic()
    };
    if let Some(server) = managed.as_mut() {
        info!("stopping managed NATS");
        if let Err(stop_error) = server.stop() {
            error!(%stop_error, "failed to stop managed NATS");
            if result.is_ok() {
                return Err(stop_error).into_diagnostic();
            }
        } else {
            info!("managed NATS stopped");
        }
    }
    result
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    let args = Args::parse();
    // Generating a bundle and renewing certificates need no server policy.
    match args.operation {
        Some(OperationArgs::Init(init)) if init.admin.is_none() => {
            let InitArgs {
                profile,
                config,
                format,
                ..
            } = *init;
            let system = profile.system;
            let root = profile.paths(|name| env::var_os(name))?.config_root;
            return setup::init_config(format, &config, system, root);
        }
        Some(OperationArgs::Tls(command)) => return setup::tls(command),
        Some(OperationArgs::Upgrade(upgrade_args)) => return upgrade(&upgrade_args),
        _ => {}
    }
    let isolated_process_group = match &args.operation {
        Some(OperationArgs::Check(check)) => check.server.isolated_process_group,
        _ => args.server.isolated_process_group,
    };
    if isolated_process_group {
        #[cfg(unix)]
        {
            // SAFETY: these calls change only this process's group. No pointers
            // or pre-exec callbacks are involved, and children inherit the group.
            let result = unsafe {
                if libc::getpgrp() == libc::getpid() {
                    0
                } else {
                    libc::setpgid(0, 0)
                }
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error()).into_diagnostic();
            }
        }
        #[cfg(not(unix))]
        return Err(miette!("--isolated-process-group requires Unix"));
    }
    let policy = StartupPolicy::resolve(args)?;
    let telemetry = telemetry::init(policy.verbose, policy.operation == Operation::Check)?;
    let result = run(policy).await;
    telemetry.shutdown().await;
    result
}

/// Check for, or install, a newer trellis-server release from GitHub. A
/// running server keeps its old binary until it is restarted.
fn upgrade(args: &UpgradeArgs) -> miette::Result<()> {
    use trellis_cli::self_update::{
        check_for_update, install_update, ReleaseChannel, SelfUpdateTarget, UpdateResult,
    };
    const TARGET: SelfUpdateTarget = SelfUpdateTarget::new(
        "oats-center",
        "trellis",
        "trellis-server",
        env!("CARGO_PKG_VERSION"),
    );
    let json = args.format == setup::OutputFormat::Json;
    let channel = ReleaseChannel::from_prerelease_flag(args.prerelease);
    if args.check {
        let check = check_for_update(TARGET, channel)?;
        if json {
            println!("{}", serde_json::to_string(&check).into_diagnostic()?);
        } else if check.needs_update {
            println!(
                "update available: {} -> {}",
                check.current_version, check.latest_version
            );
        } else {
            println!("up to date: {}", check.current_version);
        }
    } else {
        let result = install_update(TARGET, channel)?;
        if json {
            println!("{}", serde_json::to_string(&result).into_diagnostic()?);
        } else {
            match result {
                UpdateResult::UpToDate { version } => println!("up to date: {version}"),
                UpdateResult::Updated { version } => {
                    println!(
                        "updated trellis-server to {version}; restart it to run the new version"
                    )
                }
            }
        }
    }
    Ok(())
}

/// Asks a yes-or-no question on the terminal, defaulting to yes. Without a
/// terminal to ask on, the answer is no.
fn confirm(question: &str) -> miette::Result<bool> {
    use std::io::{IsTerminal as _, Write as _};

    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(false);
    }
    eprint!("{question} [Y/n] ");
    std::io::stderr().flush().into_diagnostic()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).into_diagnostic()?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    ))
}

/// Resolve the bootstrap password from argv or standard input. Stdin is preferred so the
/// secret does not appear in the process list or shell history.
fn resolve_bootstrap_password(
    password: Option<&str>,
    password_stdin: bool,
) -> miette::Result<Option<String>> {
    if let Some(password) = password {
        return Ok(Some(password.to_owned()));
    }
    if password_stdin {
        let mut password = String::new();
        std::io::stdin()
            .read_line(&mut password)
            .into_diagnostic()?;
        let password = password.trim_end_matches(['\n', '\r']).to_owned();
        if password.is_empty() {
            return Err(miette!("--password-stdin received an empty password"));
        }
        return Ok(Some(password));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn policy(arguments: &[&str]) -> StartupPolicy {
        StartupPolicy::resolve(Args::try_parse_from(arguments).expect("parse args"))
            .expect("resolve policy")
    }

    #[test]
    fn rejects_conflicts_and_does_not_swallow_mode() {
        let explicit = Args::try_parse_from([
            "trellis-server",
            "--local-nats=/x/nats-server",
            "--nats-download",
        ])
        .expect("clap accepts policy conflict");
        assert!(StartupPolicy::resolve(explicit).is_err());
        for ports in ["14222,14222,18080", "14222,18222"] {
            let parsed =
                Args::try_parse_from(["trellis-server", &format!("--local-nats-ports={ports}")])
                    .expect("parse ports");
            assert!(StartupPolicy::resolve(parsed).is_err(), "{ports}");
        }
        assert!(Args::try_parse_from(["trellis-server", "--check"]).is_err());
        assert!(Args::try_parse_from(["trellis-server", "--reset-admin", "check"]).is_err());
        assert!(Args::try_parse_from(["trellis-server", "check", "--reset-admin"]).is_err());
        assert!(Args::try_parse_from(["trellis-server", "upgrade", "--config", "x"]).is_err());
        let parsed = Args::try_parse_from(["trellis-server", "--nats-download", "jobs"])
            .expect("flag followed by mode");
        assert_eq!(parsed.mode, RuntimeMode::Jobs);
    }

    #[test]
    fn parses_run_and_check_operations_with_the_same_server_options() {
        let run = policy(&["trellis-server", "--system", "--nats-download", "jobs"]);
        let check = policy(&[
            "trellis-server",
            "check",
            "--system",
            "--nats-download",
            "jobs",
        ]);
        assert_eq!(run.operation, Operation::Run);
        assert_eq!(check.operation, Operation::Check);
        assert_eq!(run.mode, check.mode);
        assert_eq!(run.paths, check.paths);
        assert_eq!(run.nats_binary, check.nats_binary);
    }

    #[test]
    fn check_uses_injected_xdg_and_system_profiles() {
        let values = HashMap::from([
            ("HOME", "/home/test"),
            ("XDG_CONFIG_HOME", "/config"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CACHE_HOME", "/cache"),
            ("XDG_RUNTIME_DIR", "/runtime"),
        ]);
        let args = Args::try_parse_from(["trellis-server", "check"]).expect("parse check");
        let user = StartupPolicy::resolve_with(args, |name| values.get(name).map(Into::into))
            .expect("resolve user check");
        assert_eq!(user.operation, Operation::Check);
        assert_eq!(user.paths.data_root, PathBuf::from("/data/trellis"));
        assert_eq!(user.paths.runtime_root, PathBuf::from("/runtime/trellis"));

        #[cfg(target_os = "linux")]
        {
            let system = policy(&["trellis-server", "check", "--system"]);
            assert_eq!(system.paths.data_root, PathBuf::from("/var/lib/trellis"));
            assert_eq!(system.paths.runtime_root, PathBuf::from("/run/trellis"));
        }
    }

    #[test]
    fn run_and_check_resolve_identical_configured_runtime_paths() {
        let directory = tempfile::tempdir().expect("create config directory");
        let config = directory.path().join("config.toml");
        fs::write(
            &config,
            r#"
[paths]
data = "./data-root"
state = "./state-root"

[platform.storage]
kind = "sqlite"
path = "./explicit-platform.sqlite"

[jobs.storage]
kind = "sqlite"
"#,
        )
        .expect("write config");
        let config = config.to_string_lossy().into_owned();
        let run = policy(&["trellis-server", "--config", &config]);
        let check = policy(&["trellis-server", "check", "--config", &config]);
        let (run_config, run_paths) = load_runtime_config(&run).expect("load run config");
        let (check_config, check_paths) = load_runtime_config(&check).expect("load check config");

        assert_eq!(run_paths, check_paths);
        assert_eq!(run_paths.data, directory.path().join("data-root"));
        assert_eq!(run_paths.state, directory.path().join("state-root"));
        assert_eq!(run_config, check_config);
        let trellis_runtime::StorageBackend::Sqlite(platform) = run_config
            .platform_storage_backend()
            .expect("platform storage");
        assert_eq!(
            platform.path,
            directory.path().join("explicit-platform.sqlite")
        );
        let trellis_runtime::StorageBackend::Sqlite(jobs) =
            run_config.jobs_storage_backend().expect("jobs storage");
        assert_eq!(jobs.path, directory.path().join("data-root/jobs.sqlite"));
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_verifies_private_runtime_fallback() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::tempdir().expect("create fallback parent");
        let root = parent.path().join("trellis-runtime");
        ensure_private_runtime_fallback(&root).expect("create private fallback");
        let metadata = fs::symlink_metadata(&root).expect("read fallback metadata");
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_or_permissive_runtime_fallback() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let parent = tempfile::tempdir().expect("create fallback parent");
        let target = parent.path().join("target");
        fs::create_dir(&target).expect("create symlink target");
        let symlinked = parent.path().join("symlinked");
        symlink(&target, &symlinked).expect("create fallback symlink");
        assert!(ensure_private_runtime_fallback(&symlinked).is_err());

        let permissive = parent.path().join("permissive");
        fs::create_dir(&permissive).expect("create permissive fallback");
        fs::set_permissions(&permissive, fs::Permissions::from_mode(0o755))
            .expect("set permissive mode");
        assert!(ensure_private_runtime_fallback(&permissive).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn configured_roots_keep_existing_permissions_and_new_roots_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("create root directory");
        for (name, mode) in [
            ("data", 0o750),
            ("state", 0o770),
            ("runtime", 0o710),
            ("logs", 0o755),
        ] {
            let path = directory.path().join(name);
            fs::create_dir(&path).expect("create configured root");
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .expect("set configured permissions");
            prepare_directory(&path).expect("prepare existing root");
            assert_eq!(
                fs::symlink_metadata(&path)
                    .expect("inspect configured root")
                    .permissions()
                    .mode()
                    & 0o777,
                mode,
                "{name} permissions changed"
            );
        }

        let created = directory.path().join("created");
        prepare_directory(&created).expect("prepare missing root");
        assert_eq!(
            fs::symlink_metadata(&created)
                .expect("inspect created root")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn directory_preparation_rejects_regular_files() {
        let directory = tempfile::tempdir().expect("create root directory");
        let file = directory.path().join("not-a-directory");
        fs::write(&file, "not a directory").expect("write file");
        assert!(prepare_directory(&file).is_err());
    }

    #[test]
    fn resolves_xdg_and_fhs_paths() {
        let values = HashMap::from([
            ("HOME", "/home/test"),
            ("XDG_CONFIG_HOME", "/config"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CACHE_HOME", "/cache"),
            ("XDG_RUNTIME_DIR", "/runtime"),
        ]);
        let paths =
            ServerPaths::user(None, |name| values.get(name).map(Into::into)).expect("user paths");
        assert_eq!(paths.config, PathBuf::from("/config/trellis/config.toml"));
        assert_eq!(paths.data_root, PathBuf::from("/data/trellis"));
        assert_eq!(paths.state_root, PathBuf::from("/state/trellis"));
        assert_eq!(paths.cache_root, PathBuf::from("/cache/trellis"));
        assert_eq!(paths.runtime_root, PathBuf::from("/runtime/trellis"));
        assert_eq!(paths.log_root, PathBuf::from("/state/trellis/log"));
        assert_eq!(paths.config_root, PathBuf::from("/config/trellis"));

        #[cfg(target_os = "linux")]
        {
            let paths = ServerPaths::system(None).expect("system paths");
            assert_eq!(paths.config, PathBuf::from("/etc/trellis/config.toml"));
            assert_eq!(paths.data_root, PathBuf::from("/var/lib/trellis"));
            assert_eq!(paths.state_root, PathBuf::from("/var/lib/trellis"));
            assert_eq!(paths.cache_root, PathBuf::from("/var/cache/trellis"));
            assert_eq!(paths.runtime_root, PathBuf::from("/run/trellis"));
            assert_eq!(paths.log_root, PathBuf::from("/var/log/trellis"));
        }
    }

    #[test]
    fn resolves_xdg_fallbacks_and_private_runtime() {
        let paths = ServerPaths::user(None, |name| (name == "HOME").then(|| "/home/test".into()))
            .expect("fallback paths");
        assert_eq!(
            paths.config_root,
            PathBuf::from("/home/test/.config/trellis")
        );
        assert_eq!(
            paths.data_root,
            PathBuf::from("/home/test/.local/share/trellis")
        );
        assert_eq!(
            paths.state_root,
            PathBuf::from("/home/test/.local/state/trellis")
        );
        assert_eq!(paths.cache_root, PathBuf::from("/home/test/.cache/trellis"));
        assert_ne!(paths.runtime_root, paths.config_root);
        assert_ne!(paths.runtime_root, paths.data_root);
    }

    #[test]
    fn explicit_config_selects_its_sibling_nats_source() {
        let values = HashMap::from([("HOME", "/home/test")]);
        let paths = ServerPaths::user(Some(PathBuf::from("/profile/config.toml")), |name| {
            values.get(name).map(Into::into)
        })
        .expect("user paths");
        assert_eq!(paths.config, PathBuf::from("/profile/config.toml"));
        assert_eq!(paths.config_root, PathBuf::from("/profile"));
        assert_eq!(
            paths.data_root,
            PathBuf::from("/home/test/.local/share/trellis")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn explicit_system_config_keeps_fhs_defaults() {
        let paths = ServerPaths::system(Some(PathBuf::from("/opt/trellis/config.toml")))
            .expect("system paths");
        assert_eq!(paths.config_root, PathBuf::from("/opt/trellis"));
        assert_eq!(paths.data_root, PathBuf::from("/var/lib/trellis"));
        assert_eq!(paths.state_root, PathBuf::from("/var/lib/trellis"));
        assert_eq!(paths.cache_root, PathBuf::from("/var/cache/trellis"));
        assert_eq!(paths.runtime_root, PathBuf::from("/run/trellis"));
        assert_eq!(paths.log_root, PathBuf::from("/var/log/trellis"));
    }

    #[test]
    fn managed_nats_uses_selected_config_and_effective_roots() {
        let roots = RuntimePathDefaults {
            data: PathBuf::from("/roots/data"),
            state: PathBuf::from("/roots/state"),
            cache: PathBuf::from("/roots/cache"),
            runtime: PathBuf::from("/roots/runtime"),
            logs: PathBuf::from("/roots/logs"),
        };
        assert_eq!(
            ManagedNatsPaths::resolve(Path::new("/selected/config"), &roots),
            ManagedNatsPaths {
                source: PathBuf::from("/selected/config/nats"),
                state: PathBuf::from("/roots/state/nats"),
                cache: PathBuf::from("/roots/cache/nats"),
                pid: PathBuf::from("/roots/runtime/nats-server.pid"),
                log: PathBuf::from("/roots/logs/nats-server.log"),
            }
        );
    }
}
