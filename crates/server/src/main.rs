//! Command-line entrypoint and host policy for the Trellis server process.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use miette::{miette, IntoDiagnostic as _};
use tracing::{error, info};
use trellis_local_nats::{LocalNats, LocalNatsPorts, NatsBinarySource, NatsOutput};
use trellis_runtime::{
    NatsEndpointOverride, RuntimeConfig, RuntimeMode, RuntimeOptions, RuntimePathDefaults,
};

mod telemetry;

#[derive(Debug, Parser)]
#[command(version, about = "Run the Trellis server")]
struct Args {
    #[command(subcommand)]
    operation: Option<OperationArgs>,
    #[command(flatten)]
    server: ServerArgs,
    /// Issue a one-time password-reset URL for the sole active administrator.
    #[arg(long)]
    reset_admin: bool,
    /// Runtime mode to run: all, platform, jobs, health, or events.
    #[arg(default_value = "all")]
    mode: RuntimeMode,
}

#[derive(Debug, Subcommand)]
enum OperationArgs {
    /// Validate configuration and runtime dependencies, then exit.
    Check(CheckArgs),
}

#[derive(Debug, clap::Args)]
struct CheckArgs {
    /// Runtime mode to validate: all, platform, jobs, health, or events.
    #[arg(default_value = "all")]
    mode: RuntimeMode,
}

#[derive(Debug, clap::Args)]
struct ServerArgs {
    /// Override the profile's config.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Use Linux system/FHS paths instead of the default user profile.
    #[arg(long, conflicts_with = "dev", global = true)]
    system: bool,
    /// Manage local NATS from PATH, or use an exact `--local-nats=<PATH>` executable.
    #[arg(
        short = 'n',
        long,
        num_args = 0..=1,
        require_equals = true,
        global = true
    )]
    local_nats: Option<Option<PathBuf>>,
    /// Manage the explicitly downloaded, checksum-verified pinned NATS release.
    #[arg(long, global = true)]
    nats_download: bool,
    /// Managed NATS client, monitor, and websocket ports (default: 4222,8222,8080).
    #[arg(long, global = true, num_args = 1, value_delimiter = ',')]
    local_nats_ports: Option<Vec<u16>>,
    /// User-profile local development preset: PATH NATS and verbose attached logs.
    #[arg(short, long, global = true)]
    dev: bool,
    /// Enable detailed server logs and mirror managed NATS output to the terminal.
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Run,
    Check,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NatsPolicy {
    ExternalConfigured,
    Local(NatsBinarySource),
}

#[derive(Debug, Eq, PartialEq)]
struct StartupPolicy {
    operation: Operation,
    mode: RuntimeMode,
    paths: ServerPaths,
    nats: NatsPolicy,
    nats_ports: LocalNatsPorts,
    verbose: bool,
    reset_admin: bool,
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
        let (operation, server, mode, reset_admin) = match args.operation {
            Some(OperationArgs::Check(check)) => {
                if args.reset_admin {
                    return Err(miette!("--reset-admin is not valid with check"));
                }
                (Operation::Check, args.server, check.mode, false)
            }
            None => (Operation::Run, args.server, args.mode, args.reset_admin),
        };
        let paths = if server.system {
            ServerPaths::system(server.config)?
        } else {
            ServerPaths::user(server.config, get)?
        };
        let local_requested = server.local_nats.is_some();
        let explicit_path = server.local_nats.flatten();
        if explicit_path.is_some() && server.nats_download {
            return Err(miette!(
                "--local-nats=<PATH> conflicts with --nats-download"
            ));
        }
        let nats = if server.nats_download {
            NatsPolicy::Local(NatsBinarySource::DownloadPinned)
        } else if let Some(path) = explicit_path {
            NatsPolicy::Local(NatsBinarySource::Path(path))
        } else if local_requested || server.dev {
            NatsPolicy::Local(NatsBinarySource::PathLookup)
        } else {
            NatsPolicy::ExternalConfigured
        };
        if server.local_nats_ports.is_some() && matches!(nats, NatsPolicy::ExternalConfigured) {
            return Err(miette!("--local-nats-ports requires managed NATS"));
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
                LocalNatsPorts {
                    nats,
                    monitor,
                    websocket,
                }
            }
            None => LocalNatsPorts::default(),
        };
        Ok(Self {
            operation,
            mode,
            paths,
            nats,
            nats_ports,
            verbose: server.verbose || server.dev,
            reset_admin,
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
        #[cfg(not(target_os = "linux"))]
        return Err(miette!("--system is supported only on Linux"));

        #[cfg(target_os = "linux")]
        {
            let config_root = PathBuf::from("/etc/trellis");
            Ok(Self::new(
                config_root.clone(),
                PathBuf::from("/var/lib/trellis"),
                PathBuf::from("/var/lib/trellis"),
                PathBuf::from("/var/cache/trellis"),
                PathBuf::from("/run/trellis"),
                PathBuf::from("/var/log/trellis"),
                config.unwrap_or_else(|| config_root.join("config.toml")),
            ))
        }
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
        let state_root = root("XDG_STATE_HOME", ".local/state");
        let (runtime_root, runtime_fallback_root) = match get("XDG_RUNTIME_DIR") {
            Some(path) => (PathBuf::from(path).join("trellis"), None),
            None => {
                let root = private_runtime_fallback();
                (root.join("trellis"), Some(root))
            }
        };
        let mut paths = Self::new(
            config_root.clone(),
            root("XDG_DATA_HOME", ".local/share"),
            state_root.clone(),
            root("XDG_CACHE_HOME", ".cache"),
            runtime_root,
            state_root.join("log"),
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

fn private_runtime_fallback() -> PathBuf {
    #[cfg(unix)]
    let user = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let user = env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
    env::temp_dir().join(format!("trellis-runtime-{user}"))
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
        return Err(miette!(
            "Trellis configuration file not found at {}",
            policy.paths.config.display()
        ));
    }
    let (config, paths) = load_runtime_config(&policy)?;
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
    let nats_override = match &policy.nats {
        NatsPolicy::ExternalConfigured => {
            info!(config = %policy.paths.config.display(), "using configured external NATS");
            None
        }
        NatsPolicy::Local(source) => {
            let managed_paths = ManagedNatsPaths::resolve(&policy.paths.config_root, &paths);
            prepare_directory(&managed_paths.state)?;
            prepare_directory(&paths.runtime)?;
            prepare_directory(&paths.logs)?;
            if matches!(source, NatsBinarySource::DownloadPinned) {
                prepare_directory(&managed_paths.cache)?;
            }
            info!(log = %managed_paths.log.display(), "starting managed NATS");
            let server = LocalNats::builder()
                .binary(source.clone())
                .source(managed_paths.source)
                .state(managed_paths.state)
                .ports(policy.nats_ports)
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
            let websocket = server.websocket_url().to_string();
            managed = Some(server);
            Some(NatsEndpointOverride {
                servers,
                websocket: Some(websocket),
            })
        }
    };

    let result = if policy.operation == Operation::Check {
        let report = trellis_runtime::check(
            policy.mode,
            policy.paths.config.clone(),
            config,
            nats_override.as_ref().map(|value| value.servers.as_str()),
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
    let policy = StartupPolicy::resolve(Args::parse())?;
    let telemetry = telemetry::init(policy.verbose, policy.operation == Operation::Check)?;
    let result = run(policy).await;
    telemetry.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::collections::HashMap;

    fn policy(arguments: &[&str]) -> StartupPolicy {
        StartupPolicy::resolve(Args::try_parse_from(arguments).expect("parse args"))
            .expect("resolve policy")
    }

    #[test]
    fn resolves_nats_policy_before_side_effects() {
        assert_eq!(
            policy(&["trellis-server"]).nats,
            NatsPolicy::ExternalConfigured
        );
        assert_eq!(
            policy(&["trellis-server", "--local-nats"]).nats,
            NatsPolicy::Local(NatsBinarySource::PathLookup)
        );
        assert_eq!(
            policy(&["trellis-server", "--local-nats=/x/nats-server"]).nats,
            NatsPolicy::Local(NatsBinarySource::Path(PathBuf::from("/x/nats-server")))
        );
        assert_eq!(
            policy(&["trellis-server", "--nats-download"]).nats,
            NatsPolicy::Local(NatsBinarySource::DownloadPinned)
        );
        assert_eq!(
            policy(&["trellis-server", "--local-nats", "--nats-download"]).nats,
            NatsPolicy::Local(NatsBinarySource::DownloadPinned)
        );
        assert_eq!(
            policy(&[
                "trellis-server",
                "--local-nats",
                "--local-nats-ports=14222,18222,18080",
            ])
            .nats_ports,
            LocalNatsPorts {
                nats: 14222,
                monitor: 18222,
                websocket: 18080,
            }
        );
        let dev = policy(&["trellis-server", "--dev"]);
        assert_eq!(dev.nats, NatsPolicy::Local(NatsBinarySource::PathLookup));
        assert!(dev.verbose);
        let dev_download = policy(&["trellis-server", "--dev", "--nats-download"]);
        assert_eq!(
            dev_download.nats,
            NatsPolicy::Local(NatsBinarySource::DownloadPinned)
        );
        assert!(dev_download.verbose);
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
        assert!(StartupPolicy::resolve(
            Args::try_parse_from(["trellis-server", "--local-nats-ports=14222,18222,18080"])
                .expect("parse ports")
        )
        .is_err());
        assert!(StartupPolicy::resolve(
            Args::try_parse_from([
                "trellis-server",
                "--local-nats",
                "--local-nats-ports=14222,14222,18080",
            ])
            .expect("parse duplicate ports")
        )
        .is_err());
        assert!(StartupPolicy::resolve(
            Args::try_parse_from([
                "trellis-server",
                "--local-nats",
                "--local-nats-ports=14222,18222",
            ])
            .expect("parse incomplete ports")
        )
        .is_err());
        let error = Args::try_parse_from(["trellis-server", "--dev", "--system"])
            .expect_err("dev and system conflict");
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        assert!(Args::try_parse_from(["trellis-server", "--check"]).is_err());
        let parsed = Args::try_parse_from(["trellis-server", "--reset-admin", "check"])
            .expect("parse check after run-only option");
        assert!(StartupPolicy::resolve(parsed).is_err());
        assert!(Args::try_parse_from(["trellis-server", "check", "--reset-admin"]).is_err());
        let parsed = Args::try_parse_from(["trellis-server", "--local-nats", "jobs"])
            .expect("bare local nats followed by mode");
        assert_eq!(parsed.mode, RuntimeMode::Jobs);
        assert_eq!(parsed.server.local_nats, Some(None));
    }

    #[test]
    fn parses_run_and_check_operations_with_the_same_server_options() {
        let run = policy(&["trellis-server", "--system", "--local-nats", "jobs"]);
        let check = policy(&[
            "trellis-server",
            "check",
            "--system",
            "--local-nats",
            "jobs",
        ]);
        assert_eq!(run.operation, Operation::Run);
        assert_eq!(check.operation, Operation::Check);
        assert_eq!(run.mode, check.mode);
        assert_eq!(run.paths, check.paths);
        assert_eq!(run.nats, check.nats);

        for (before, after) in [
            (
                &["trellis-server", "--system", "check"][..],
                &["trellis-server", "check", "--system"][..],
            ),
            (
                &["trellis-server", "--config", "/x/config.toml", "check"][..],
                &["trellis-server", "check", "--config", "/x/config.toml"][..],
            ),
            (
                &["trellis-server", "--local-nats", "check"][..],
                &["trellis-server", "check", "--local-nats"][..],
            ),
            (
                &["trellis-server", "--local-nats=/x/nats-server", "check"][..],
                &["trellis-server", "check", "--local-nats=/x/nats-server"][..],
            ),
            (
                &["trellis-server", "--nats-download", "check"][..],
                &["trellis-server", "check", "--nats-download"][..],
            ),
            (
                &["trellis-server", "--dev", "check"][..],
                &["trellis-server", "check", "--dev"][..],
            ),
            (
                &["trellis-server", "--verbose", "check"][..],
                &["trellis-server", "check", "--verbose"][..],
            ),
        ] {
            assert_eq!(policy(before), policy(after));
        }
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

    #[test]
    fn check_preserves_every_managed_nats_acquisition_policy() {
        assert_eq!(
            policy(&["trellis-server", "check", "--local-nats"]).nats,
            NatsPolicy::Local(NatsBinarySource::PathLookup)
        );
        assert_eq!(
            policy(&["trellis-server", "check", "--local-nats=/x/nats-server"]).nats,
            NatsPolicy::Local(NatsBinarySource::Path(PathBuf::from("/x/nats-server")))
        );
        assert_eq!(
            policy(&["trellis-server", "check", "--nats-download"]).nats,
            NatsPolicy::Local(NatsBinarySource::DownloadPinned)
        );
        let dev = policy(&["trellis-server", "check", "--dev"]);
        assert_eq!(dev.nats, NatsPolicy::Local(NatsBinarySource::PathLookup));
        assert!(dev.verbose);
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
