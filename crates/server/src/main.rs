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

mod commands;
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
    /// Issue a one-time password-reset URL for the sole active administrator.
    #[arg(long)]
    reset_admin: bool,
    /// Runtime mode to run: all, platform, jobs, health, or events.
    #[arg(default_value = "all", value_parser = parse_mode)]
    mode: RuntimeMode,
    /// How a first-run installation reports its result.
    #[arg(long, value_enum, default_value_t)]
    format: commands::OutputFormat,
}

/// Parses the runtime mode. A subcommand name lands here when options came
/// before it, so say where they go instead of listing the modes.
fn parse_mode(value: &str) -> Result<RuntimeMode, String> {
    use clap::CommandFactory as _;

    value
        .parse()
        .map_err(|error: trellis_runtime::RuntimeModeParseError| {
            if Args::command().find_subcommand(value).is_some() {
                format!("options go after the subcommand: trellis-server {value} [OPTIONS]")
            } else {
                error.to_string()
            }
        })
}

#[derive(Debug, Subcommand)]
enum OperationArgs {
    /// Validate configuration and runtime dependencies, then exit.
    Check(CheckArgs),
    /// Create the first local administrator in the configured storage, then exit.
    Admin(AdminArgs),
    /// Configuration files.
    #[command(subcommand)]
    Config(commands::ConfigSubcommand),
    /// Certificates: issue test certificates, or show generated TLS.
    #[command(subcommand)]
    Tls(commands::TlsSubcommand),
    /// NATS: make an operator and cluster, or run the `nats` CLI as one of
    /// the installation's users.
    Nats(commands::NatsArgs),
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
    format: commands::OutputFormat,
}

#[derive(Debug, clap::Args)]
struct AdminArgs {
    #[command(flatten)]
    profile: InstallationArgs,
    #[command(flatten)]
    bootstrap: BootstrapAdminArgs,
    /// Print the result as text or JSON.
    #[arg(long, value_enum, default_value_t)]
    format: commands::OutputFormat,
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

/// Which installation a command uses.
#[derive(Debug, Default, clap::Args)]
pub(crate) struct InstallationArgs {
    /// An installation folder holding config.toml, which keeps what the
    /// server creates in its `var` folder, or a config.toml. Omitted, the
    /// user's installation, or the system's for root on Linux.
    #[arg(long, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,
    /// Use the user's installation (~/.config/trellis), even as root.
    #[arg(long, conflicts_with_all = ["config", "system"])]
    pub(crate) user: bool,
    /// Use the system installation (/etc/trellis), Linux only.
    #[arg(long, conflicts_with = "config")]
    pub(crate) system: bool,
}

impl InstallationArgs {
    fn location(&self) -> Location {
        match &self.config {
            Some(path) => Location::Bundle(path.clone()),
            None => Location::profile(self.user, self.system),
        }
    }

    fn paths(
        &self,
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> miette::Result<ServerPaths> {
        ServerPaths::resolve(&self.location(), get)
    }
}

/// An installation's kind and where it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Location {
    /// `~/.config/trellis`, with data in the user's XDG folders.
    User,
    /// `/etc/trellis`, with data in the FHS folders.
    System,
    /// A self-contained folder, or the config.toml in one.
    Bundle(PathBuf),
}

impl Location {
    /// The named profile, or root's default: the system installation on Linux.
    fn profile(user: bool, system: bool) -> Self {
        #[cfg(target_os = "linux")]
        // SAFETY: geteuid has no arguments or borrowed memory.
        let root = unsafe { libc::geteuid() } == 0;
        #[cfg(not(target_os = "linux"))]
        let root = false;
        if system || (root && !user) {
            Self::System
        } else {
            Self::User
        }
    }
}

/// Options shared by starting and checking a server.
#[derive(Debug, Default, clap::Args)]
struct ServerArgs {
    #[command(flatten)]
    profile: InstallationArgs,
    /// Run a development installation, created the first time: Trellis runs
    /// its own NATS server, and serves plain HTTP on this machine, or with
    /// --host HTTPS from a CA it makes. Kept in --config, or in the user's
    /// data folder.
    #[arg(long)]
    dev: bool,
    /// With --dev, the hostname other machines reach this one by.
    #[arg(long, requires = "dev")]
    host: Option<String>,
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
    location: Location,
    dev: Option<Option<String>>,
    nats_binary: NatsBinary,
    nats_ports: Option<LocalNatsPorts>,
    nats_cache: Option<PathBuf>,
    advertised_native: Option<Vec<String>>,
    advertised_websocket: Option<Vec<String>>,
    verbose: bool,
    reset_admin: bool,
    bootstrap_admin: Option<BootstrapAdminArgs>,
    format: commands::OutputFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerPaths {
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
            Some(OperationArgs::Admin(admin)) => (
                Operation::BootstrapAdmin,
                ServerArgs {
                    profile: admin.profile,
                    ..ServerArgs::default()
                },
                args.mode,
                Some(admin.bootstrap),
                admin.format,
            ),
            Some(
                OperationArgs::Tls(_)
                | OperationArgs::Nats(_)
                | OperationArgs::Config(_)
                | OperationArgs::Upgrade(_),
            ) => {
                return Err(miette!(
                    "this trellis-server command runs without a server policy"
                ));
            }
            None => (Operation::Run, args.server, args.mode, None, args.format),
        };
        // A development installation is a bundle of its own, in the user's
        // data folder unless --config names one.
        let location = match (&server.profile.config, server.dev) {
            (None, true) => Location::Bundle(
                RuntimePathDefaults::user_profile(&get)
                    .into_diagnostic()?
                    .data
                    .join("dev"),
            ),
            _ => server.profile.location(),
        };
        let dev = server.dev.then(|| server.host.clone());
        let paths = ServerPaths::resolve(&location, get)?;
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
            location,
            dev,
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
    RuntimeConfig::load_from_path_with_defaults(&policy.paths.config, policy.paths.defaults())
        .into_diagnostic()
}

impl ServerPaths {
    pub(crate) fn resolve(
        location: &Location,
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> miette::Result<Self> {
        match location {
            Location::System => Self::system(),
            Location::User => Self::user(get),
            Location::Bundle(path) => Self::bundle(path, get),
        }
    }

    fn system() -> miette::Result<Self> {
        let defaults = RuntimePathDefaults::system_profile().into_diagnostic()?;
        let config_root = PathBuf::from("/etc/trellis");
        Ok(Self::new(
            config_root.clone(),
            defaults,
            config_root.join("config.toml"),
        ))
    }

    fn user(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> miette::Result<Self> {
        let home = get("HOME")
            .or_else(|| get("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| miette!("cannot resolve user profile: HOME is not set"))?;
        let config_root = get("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("trellis");
        let defaults = RuntimePathDefaults::user_profile(&get).into_diagnostic()?;
        let runtime_fallback_root = if get("XDG_RUNTIME_DIR").is_none() {
            defaults.runtime.parent().map(Path::to_path_buf)
        } else {
            None
        };
        let mut paths = Self::new(
            config_root.clone(),
            defaults,
            config_root.join("config.toml"),
        );
        paths.runtime_fallback_root = runtime_fallback_root;
        Ok(paths)
    }

    /// A self-contained installation keeps what the server creates in its
    /// `var` folder, so deleting `var` returns it to just after `init`. The
    /// NATS download goes in the shared cache, so bundles download it once.
    fn bundle(
        path: &Path,
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> miette::Result<Self> {
        let path = std::path::absolute(path).into_diagnostic()?;
        let (root, config) = if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            let root = path.parent().unwrap_or(Path::new("/")).to_path_buf();
            (root, path)
        } else {
            (path.clone(), path.join("config.toml"))
        };
        let var = root.join("var");
        let cache = match Location::profile(false, false) {
            Location::System => {
                RuntimePathDefaults::system_profile()
                    .into_diagnostic()?
                    .cache
            }
            _ => {
                RuntimePathDefaults::user_profile(&get)
                    .into_diagnostic()?
                    .cache
            }
        };
        Ok(Self::new(
            root,
            RuntimePathDefaults {
                data: var.clone(),
                state: var.clone(),
                cache,
                runtime: var.join("run"),
                logs: var.join("log"),
            },
            config,
        ))
    }

    /// The data folders this installation uses unless its `[paths]` says otherwise.
    pub(crate) fn defaults(&self) -> RuntimePathDefaults {
        RuntimePathDefaults {
            data: self.data_root.clone(),
            state: self.state_root.clone(),
            cache: self.cache_root.clone(),
            runtime: self.runtime_root.clone(),
            logs: self.log_root.clone(),
        }
    }

    fn new(config_root: PathBuf, defaults: RuntimePathDefaults, config: PathBuf) -> Self {
        Self {
            config_root,
            data_root: defaults.data,
            state_root: defaults.state,
            cache_root: defaults.cache,
            runtime_root: defaults.runtime,
            log_root: defaults.logs,
            config,
            runtime_fallback_root: None,
        }
    }
}

impl ManagedNatsPaths {
    fn resolve(config_root: &Path, paths: &RuntimePathDefaults) -> Self {
        Self {
            source: config_root.to_path_buf(),
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
        match &policy.dev {
            Some(host) => {
                commands::create_dev_installation(&policy.paths.config_root, host.as_deref())?;
                info!(folder = %policy.paths.config_root.display(), "created a development installation");
            }
            None => {
                return Err(miette!(
                    "no Trellis configuration at {}; write one there (`trellis-server config \
                    example` prints a commented starting point), name one with --config, or \
                    try Trellis with `trellis-server --dev`",
                    policy.paths.config.display()
                ))
            }
        }
    }
    let (mut config, paths) = load_runtime_config(&policy)?;

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
    let mut managed = None;
    // Seeding an administrator only touches the database, so it never needs
    // NATS, which a development installation runs only while Trellis does.
    let seeding = policy.operation == Operation::BootstrapAdmin;
    let managed_nats = config.nats.as_ref().is_some_and(|nats| nats.managed) && !seeding;
    if managed_nats && policy.dev.is_none() {
        return Err(miette!(
            "{} has [nats] managed, which only a development installation may: start it \
            with --dev, or point [nats] servers at NATS servers you run",
            policy.paths.config.display()
        ));
    }
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
                    runtime_servers: None,
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
            let relayed = config
                .http
                .as_ref()
                .is_some_and(|http| http.nats_websocket_upstream.is_some());
            let server = LocalNats::builder()
                .binary(source)
                .relayed_websocket(relayed)
                // Started from the main thread, which outlives the broker.
                .own_process_group(true)
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
            // Provisioning below reaches the broker where it listens, which
            // --local-nats-ports may have moved.
            if let Some(nats) = config.nats.as_mut() {
                nats.servers = Some(servers.clone());
            }
            // Clients elsewhere reach the broker by the addresses [client]
            // names, not the loopback address the runtime itself uses.
            let client_native = config.client.as_ref().and_then(|c| c.nats_servers.clone());
            let client_websocket = config
                .client
                .as_ref()
                .and_then(|c| c.ws_nats_servers.clone());
            // A runtime relaying browser WebSockets relays to the broker it
            // just started, and keeps advertising its own origin.
            let websocket = match config.http.as_mut() {
                Some(http) if http.nats_websocket_upstream.is_some() => {
                    http.nats_websocket_upstream =
                        Some(format!("127.0.0.1:{}", nats_ports.websocket));
                    config.same_origin_websocket_url().map(|url| vec![url])
                }
                _ => client_websocket.or_else(|| Some(vec![server.websocket_url().to_string()])),
            };
            managed = Some(server);
            Some(NatsEndpointOverride {
                runtime_servers: Some(servers.clone()),
                advertised_native: policy
                    .advertised_native
                    .clone()
                    .or(client_native)
                    .or_else(|| Some(vec![servers])),
                advertised_websocket: policy.advertised_websocket.clone().or(websocket),
            })
        }
    };

    // Keys, Trellis's NATS accounts and generated TLS, the first time and on
    // every start after; the NATS servers are reachable by now.
    trellis_runtime::installation::prepare(
        &mut config,
        &policy.paths.config_root,
        &paths.data,
        policy.mode,
        !seeding,
    )
    .await
    .into_diagnostic()?;
    if policy.operation == Operation::Run {
        config.validate_for_mode(policy.mode).into_diagnostic()?;
    }
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
        if policy.format == commands::OutputFormat::Json {
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
    let renewal = (policy.operation == Operation::Run).then(|| {
        let config = config.clone();
        let data = paths.data.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(12 * 60 * 60));
            loop {
                interval.tick().await;
                if let Err(renew_error) =
                    trellis_runtime::installation::renew_generated_tls(&config, &data)
                {
                    error!(%renew_error, "failed to renew generated TLS");
                }
            }
        })
    });
    let result = if policy.operation == Operation::Check {
        let report = trellis_runtime::check(
            policy.mode,
            policy.paths.config.clone(),
            config,
            nats_override
                .as_ref()
                .and_then(|value| value.runtime_servers.as_deref()),
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
    if let Some(renewal) = renewal {
        renewal.abort();
    }
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
        Some(OperationArgs::Config(commands::ConfigSubcommand::Example)) => {
            print!("{}", commands::CONFIG_EXAMPLE);
            return Ok(());
        }
        Some(OperationArgs::Tls(command)) => return commands::tls(command),
        Some(OperationArgs::Nats(nats)) => return commands::nats(nats),
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
    let json = args.format == commands::OutputFormat::Json;
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

/// Whether there's a terminal to ask questions on.
fn interactive() -> bool {
    use std::io::IsTerminal as _;

    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Asks a yes-or-no question on the terminal, defaulting to yes. Without a
/// terminal to ask on, the answer is no.
fn confirm(question: &str) -> miette::Result<bool> {
    use std::io::Write as _;

    if !interactive() {
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
        let paths = ServerPaths::user(|name| values.get(name).map(Into::into)).expect("user paths");
        assert_eq!(paths.config, PathBuf::from("/config/trellis/config.toml"));
        assert_eq!(paths.data_root, PathBuf::from("/data/trellis"));
        assert_eq!(paths.state_root, PathBuf::from("/state/trellis"));
        assert_eq!(paths.cache_root, PathBuf::from("/cache/trellis"));
        assert_eq!(paths.runtime_root, PathBuf::from("/runtime/trellis"));
        assert_eq!(paths.log_root, PathBuf::from("/state/trellis/log"));
        assert_eq!(paths.config_root, PathBuf::from("/config/trellis"));

        #[cfg(target_os = "linux")]
        {
            let paths = ServerPaths::system().expect("system paths");
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
        let paths = ServerPaths::user(|name| (name == "HOME").then(|| "/home/test".into()))
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
    fn a_bundle_is_named_by_its_folder_or_its_config_and_keeps_its_data_inside() {
        let values = HashMap::from([("HOME", "/home/test")]);
        let get = |name: &str| values.get(name).map(Into::into);
        let by_folder = ServerPaths::resolve(&Location::Bundle("/srv/dev1".into()), get).unwrap();
        let by_config =
            ServerPaths::resolve(&Location::Bundle("/srv/dev1/config.toml".into()), get).unwrap();
        assert_eq!(by_folder, by_config);
        assert_eq!(by_folder.config, PathBuf::from("/srv/dev1/config.toml"));
        for root in [
            &by_folder.data_root,
            &by_folder.state_root,
            &by_folder.runtime_root,
            &by_folder.log_root,
        ] {
            assert!(root.starts_with("/srv/dev1/var"), "{}", root.display());
        }
        // Every bundle shares one download of NATS.
        assert!(!by_folder.cache_root.starts_with("/srv/dev1"));

        for profile in ["--user", "--system"] {
            assert!(
                Args::try_parse_from(["trellis-server", profile, "--config", "/srv/dev1"]).is_err()
            );
            assert!(Args::try_parse_from([
                "trellis-server",
                "check",
                profile,
                "--config",
                "/srv/dev1"
            ])
            .is_err());
        }
    }
}
