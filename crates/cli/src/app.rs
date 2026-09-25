use std::env;
use std::io;

use crate::cli::*;
use crate::package;
use crate::self_update::{ReleaseChannel, SelfUpdateTarget};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use ed25519_dalek::SigningKey;
use miette::IntoDiagnostic;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use tracing_subscriber::EnvFilter;
use trellis_rs::auth as authlib;
use trellis_rs::client::TrellisClientError;
use trellis_rs::generated::Client;
use trellis_rs::telemetry::{
    self, instruments::DurationFamily, lifecycle::Observation, KeyValue, TelemetryGuard,
    TelemetryIdentity, TelemetryRole,
};

mod auth;
mod bootstrap;
pub mod deploy;
mod events;
mod resources;
mod runtime;
mod self_cmd;

const SELF_UPDATE_TARGET: SelfUpdateTarget = SelfUpdateTarget::new(
    "oats-center",
    "trellis",
    "trellis",
    env!("CARGO_PKG_VERSION"),
);

pub async fn run() -> miette::Result<()> {
    let cli = Cli::parse();
    let format = cli.format;
    // Completion, version, and help stay offline and never start exporters.
    let command_label = cli.command.telemetry_label();
    let telemetry_guard = if command_label.is_some() {
        telemetry::init_from_env(TelemetryIdentity::new(
            "trellis-cli",
            TelemetryRole::Cli,
            env!("CARGO_PKG_VERSION"),
        ))
    } else {
        TelemetryGuard::disabled()
    };
    init_tracing(cli.verbose, &telemetry_guard)?;
    let observation = command_label.map(|label| {
        Observation::start(
            DurationFamily::Cli,
            vec![KeyValue::new("trellis.command", label)],
            "cancelled",
        )
    });

    let result = dispatch(cli.command, format).await;

    if let Some(observation) = observation {
        observation.finish(if result.is_ok() { "ok" } else { "error" });
    }
    telemetry_guard.force_flush().await;
    result
}

async fn dispatch(command: TopLevelCommand, format: OutputFormat) -> miette::Result<()> {
    match command {
        TopLevelCommand::Add(args) => package::add(format, &args).await?,
        TopLevelCommand::Rm(args) => package::remove(format, &args).await?,
        TopLevelCommand::Check(args) => package::check(format, &args).await?,
        TopLevelCommand::Update(args) => package::update(format, &args).await?,
        TopLevelCommand::Install(args) => package::install(format, &args).await?,
        TopLevelCommand::Generate(args) => crate::generate::run(&args)?,
        TopLevelCommand::Publish(args) => package::publish(format, &args).await?,
        TopLevelCommand::Login(args) => auth::login(format, &args).await?,
        TopLevelCommand::Logout => auth::logout(format).await?,
        TopLevelCommand::Whoami => auth::whoami(format).await?,
        TopLevelCommand::Identity(command) => auth::identity(format, command).await?,
        TopLevelCommand::Participants(command) => auth::participants(format, command).await?,
        TopLevelCommand::Issuers(command) => auth::issuers(format, command).await?,
        TopLevelCommand::Users(command) => auth::users(format, command).await?,
        TopLevelCommand::Portals(command) => auth::portals(format, command).await?,
        TopLevelCommand::Svc(command) => deploy::run_svc(format, command).await?,
        TopLevelCommand::Dev(command) => deploy::run_dev(format, command).await?,
        TopLevelCommand::Resources(command) => resources::run(format, command).await?,
        TopLevelCommand::Events(command) => events::run(command).await?,
        TopLevelCommand::Init(command) => bootstrap::init(format, command).await?,
        TopLevelCommand::Keys(command) => match command.command {
            KeysSubcommand::New(args) => runtime::keygen_command(format, &args)?,
        },
        TopLevelCommand::Upgrade(command) => self_cmd::run_upgrade(format, command)?,
        TopLevelCommand::Completion { shell } => {
            let mut command = Cli::command();
            generate(shell, &mut command, "trellis", &mut io::stdout());
        }
        TopLevelCommand::Version => runtime::version_command(format)?,
    }

    Ok(())
}

fn init_tracing(verbose: u8, telemetry_guard: &TelemetryGuard) -> miette::Result<()> {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::Layer as _;

    let filter = EnvFilter::new(tracing_filter(verbose));
    let otel_layer: Option<
        Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>,
    > = telemetry_guard.tracer().map(|tracer| {
        Box::new(tracing_opentelemetry::layer().with_tracer(tracer))
            as Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>
    });

    // The console filter bounds fmt output only; instrumented Trellis spans
    // reach the OTel layer independently of the CLI verbose level.
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(io::stderr)
                .with_filter(filter),
        )
        .try_init()
        .map_err(|error| miette::miette!(error.to_string()))?;
    Ok(())
}

fn tracing_filter(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug,async_nats=info",
        _ => "trace",
    }
}

pub(crate) fn base64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) async fn connect_authenticated_cli_client(
) -> miette::Result<(authlib::AdminSessionState, Client)> {
    let state = authlib::load_admin_session().into_diagnostic()?;

    let connected = match authlib::connect_admin_client_async(&state).await {
        Ok(connected) => connected,
        Err(error) => return Err(map_admin_session_error(error)),
    };

    match auth::current_user(&connected).await {
        Ok(_) => {}
        Err(error) => return Err(map_admin_session_error(error)),
    }

    Ok((state, connected))
}

fn map_admin_session_error(error: authlib::TrellisAuthError) -> miette::Report {
    match rejected_admin_session_error_report(&error) {
        Ok(Some(report)) => report,
        Ok(None) if is_admin_session_authorization_violation_error(&error) => {
            generic_admin_authorization_violation_report()
        }
        Ok(None) => miette::miette!(error.to_string()),
        Err(report) => report,
    }
}

fn rejected_admin_session_error_report(
    error: &authlib::TrellisAuthError,
) -> miette::Result<Option<miette::Report>> {
    if is_rejected_admin_session_error(error) {
        Ok(Some(rejected_admin_session_report()?))
    } else {
        Ok(None)
    }
}

fn is_rejected_admin_session_error(error: &authlib::TrellisAuthError) -> bool {
    admin_session_error_code(error).is_some_and(|code| {
        matches!(
            code.as_str(),
            "session_not_found" | "session_expired" | "session_revoked"
        )
    })
}

fn is_admin_session_authorization_violation_error(error: &authlib::TrellisAuthError) -> bool {
    admin_session_error_code(error).as_deref() == Some("authorization_violation")
}

fn admin_session_error_code(error: &authlib::TrellisAuthError) -> Option<String> {
    match error {
        authlib::TrellisAuthError::AuthRequestHttpFailure(_, code)
        | authlib::TrellisAuthError::BindHttpFailure(_, code)
        | authlib::TrellisAuthError::TrellisClient(TrellisClientError::BootstrapHttp {
            code,
            ..
        }) => Some(code.clone()),
        authlib::TrellisAuthError::TrellisClient(TrellisClientError::RpcError(payload)) => {
            serde_json::from_str::<Value>(payload.raw())
                .ok()
                .and_then(|payload| payload.get("code")?.as_str().map(str::to_owned))
        }
        _ => None,
    }
}

fn generic_admin_authorization_violation_report() -> miette::Report {
    miette::miette!(
        "Authorization was denied by the server. Saved login credentials were retained; ask an administrator to review the participant's grants."
    )
}

fn rejected_admin_session_report() -> miette::Result<miette::Report> {
    let cleared = authlib::clear_admin_session().into_diagnostic()?;
    let message = if cleared {
        "Saved agent session was rejected by the server and the stored local session was cleared; run `trellis auth login` explicitly."
    } else {
        "Saved agent session was rejected by the server; run `trellis auth login` explicitly."
    };
    Ok(miette::miette!(message))
}

pub(crate) fn generate_session_keypair() -> (String, String) {
    let seed: [u8; 32] = rand::random();
    let signing_key = SigningKey::from_bytes(&seed);
    let public_key = signing_key.verifying_key().to_bytes();
    (base64url_encode(&seed), base64url_encode(&public_key))
}

pub(crate) fn json_value_label(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

pub(crate) fn wire<T: DeserializeOwned>(value: impl Serialize) -> miette::Result<T> {
    serde_json::from_value(serde_json::to_value(value).into_diagnostic()?).into_diagnostic()
}

pub(crate) fn wire_u64(value: impl Serialize) -> miette::Result<u64> {
    wire::<String>(value)?.parse().into_diagnostic()
}

pub(crate) fn release_channel(prerelease: bool) -> ReleaseChannel {
    ReleaseChannel::from_prerelease_flag(prerelease)
}

#[cfg(test)]
mod tests {
    use super::{
        is_rejected_admin_session_error, map_admin_session_error,
        rejected_admin_session_error_report, rejected_admin_session_report, tracing_filter,
    };
    use std::env;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};
    use trellis_rs::auth::{save_admin_session, AdminSessionState, TrellisAuthError};
    use trellis_rs::client::{RpcErrorPayload, TrellisClientError};

    fn config_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_test_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        env::temp_dir().join(format!("trellis-cli-{label}-{nanos}"))
    }

    fn admin_session_path(root: &Path) -> std::path::PathBuf {
        root.join("trellis").join("admin-session.json")
    }

    fn test_admin_session_state() -> AdminSessionState {
        AdminSessionState {
            participant_id: "trellis-app.cli@v1".to_string(),
            login_session_id: ulid::Ulid::new().to_string(),
            trellis_url: "http://localhost:3000".to_string(),
            session_seed: "seed".to_string(),
            expires_at: Some(1_767_225_600_000),
        }
    }

    #[test]
    fn verbosity_controls_dependency_noise() {
        assert_eq!(tracing_filter(0), "warn");
        assert_eq!(tracing_filter(1), "info");
        assert_eq!(tracing_filter(2), "debug,async_nats=info");
        assert_eq!(tracing_filter(3), "trace");
    }

    #[test]
    fn does_not_treat_generic_connect_authorization_violation_as_rejected_session() {
        let error = TrellisAuthError::TrellisClient(TrellisClientError::NatsConnect(
            "authorization violation".to_string(),
        ));

        assert!(!is_rejected_admin_session_error(&error));
    }

    #[test]
    fn does_not_treat_generic_request_authorization_violation_as_rejected_session() {
        let error = TrellisAuthError::TrellisClient(TrellisClientError::NatsRequest(
            "authorization violation".to_string(),
        ));

        assert!(!is_rejected_admin_session_error(&error));
    }

    #[test]
    fn does_not_treat_generic_rpc_authorization_violation_as_rejected_session() {
        let error = TrellisAuthError::TrellisClient(TrellisClientError::RpcError(
            RpcErrorPayload::from_message("authorization violation"),
        ));

        assert!(!is_rejected_admin_session_error(&error));
    }

    #[test]
    fn does_not_treat_mixed_case_authorization_violation_as_rejected_session() {
        let error = TrellisAuthError::TrellisClient(TrellisClientError::NatsRequest(
            "Authorization Violation".to_string(),
        ));

        assert!(!is_rejected_admin_session_error(&error));
    }

    #[test]
    fn rejected_session_report_clears_local_session_and_requires_explicit_login() {
        let _guard = config_env_lock().lock().expect("lock config env");
        let test_dir = unique_test_dir("rejected-session-report");
        fs::create_dir_all(test_dir.join("trellis")).expect("create test config dir");
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &test_dir);
        }

        save_admin_session(&test_admin_session_state()).expect("save admin session");
        assert!(admin_session_path(&test_dir).exists());

        let report = rejected_admin_session_report().expect("build rejected-session report");
        assert!(!admin_session_path(&test_dir).exists());
        assert!(report
            .to_string()
            .contains("run `trellis auth login` explicitly"));

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
        let _ = fs::remove_dir_all(test_dir);
    }

    #[test]
    fn authorization_denial_preserves_local_login_credentials() {
        let _guard = config_env_lock().lock().expect("lock config env");
        let test_dir = unique_test_dir("generic-rejected-session-request-error");
        fs::create_dir_all(test_dir.join("trellis")).expect("create test config dir");
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &test_dir);
        }

        save_admin_session(&test_admin_session_state()).expect("save admin session");
        assert!(admin_session_path(&test_dir).exists());

        let saved = fs::read(admin_session_path(&test_dir)).expect("read saved login");
        let error =
            TrellisAuthError::AuthRequestHttpFailure(403, "authorization_violation".to_string());
        assert!(rejected_admin_session_error_report(&error)
            .expect("map generic authorization request error")
            .is_none());
        let report = map_admin_session_error(error);

        assert_eq!(
            fs::read(admin_session_path(&test_dir)).expect("read retained login"),
            saved
        );
        assert!(report
            .to_string()
            .contains("review the participant's grants"));
        assert!(!report.to_string().contains("trellis auth login"));

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
        let _ = fs::remove_dir_all(test_dir);
    }

    #[test]
    fn mapped_rejected_session_result_clears_local_session_and_requires_explicit_login() {
        let _guard = config_env_lock().lock().expect("lock config env");
        let test_dir = unique_test_dir("mapped-rejected-session-result");
        fs::create_dir_all(test_dir.join("trellis")).expect("create test config dir");
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &test_dir);
        }

        save_admin_session(&test_admin_session_state()).expect("save admin session");
        assert!(admin_session_path(&test_dir).exists());

        let error = TrellisAuthError::BindHttpFailure(401, "session_revoked".to_string());
        let report = map_admin_session_error(error);

        assert!(!admin_session_path(&test_dir).exists());
        assert!(report
            .to_string()
            .contains("run `trellis auth login` explicitly"));

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
        let _ = fs::remove_dir_all(test_dir);
    }

    fn assert_rejected_session_error_clears_local_session(label: &str, error: TrellisAuthError) {
        let _guard = config_env_lock().lock().expect("lock config env");
        let test_dir = unique_test_dir(label);
        fs::create_dir_all(test_dir.join("trellis")).expect("create test config dir");
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &test_dir);
        }

        save_admin_session(&test_admin_session_state()).expect("save admin session");
        assert!(admin_session_path(&test_dir).exists());

        let report = map_admin_session_error(error);

        assert!(!admin_session_path(&test_dir).exists());
        assert!(report
            .to_string()
            .contains("run `trellis auth login` explicitly"));

        unsafe {
            env::remove_var("XDG_CONFIG_HOME");
        }
        let _ = fs::remove_dir_all(test_dir);
    }

    #[test]
    fn explicit_session_not_found_rejected_session_clears_local_session() {
        assert_rejected_session_error_clears_local_session(
            "session-not-found-rejected-session",
            TrellisAuthError::AuthRequestHttpFailure(401, "session_not_found".to_string()),
        );
    }

    #[test]
    fn explicit_revoked_rejected_session_clears_local_session() {
        assert_rejected_session_error_clears_local_session(
            "revoked-rejected-session",
            TrellisAuthError::BindHttpFailure(401, "session_revoked".to_string()),
        );
    }

    #[test]
    fn expired_session_clears_local_session() {
        assert_rejected_session_error_clears_local_session(
            "expired-session",
            TrellisAuthError::AuthRequestHttpFailure(401, "session_expired".to_string()),
        );
    }
}
