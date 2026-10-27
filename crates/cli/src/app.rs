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
use trellis_rs::generated::Client;
use trellis_rs::telemetry::{
    self, instruments::DurationFamily, lifecycle::Observation, KeyValue, TelemetryGuard,
    TelemetryIdentity, TelemetryRole,
};

mod auth;
pub mod deploy;
mod events;
mod native_auth;
mod resources;
mod runtime;
mod self_cmd;
mod trust;

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
        TopLevelCommand::Issuers(command) => auth::issuers(format, command).await?,
        TopLevelCommand::Users(command) => auth::users(format, command).await?,
        TopLevelCommand::Roles(command) => auth::roles(format, command).await?,
        TopLevelCommand::Clients(command) => auth::clients(command).await?,
        TopLevelCommand::Apis(command) => auth::apis(command).await?,
        TopLevelCommand::Theme(command) => auth::theme(command).await?,
        TopLevelCommand::SignIn(command) => auth::sign_in(command).await?,
        TopLevelCommand::Svc(command) => deploy::run_svc(format, command).await?,
        TopLevelCommand::Dev(command) => deploy::run_dev(format, command).await?,
        TopLevelCommand::Resources(command) => resources::run(format, command).await?,
        TopLevelCommand::Events(command) => events::run(command).await?,
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

async fn connect_authenticated_cli_client() -> miette::Result<(native_auth::Selection, Client)> {
    let selection = native_auth::Selection::load()?;
    let session = selection.open().await?.resume().await.into_diagnostic()?;
    let connected = Client::connect_user(trellis_rs::client::UserConnectOptions::new(
        selection.nats_servers.clone(),
        10_000,
        session,
    ))
    .await
    .into_diagnostic()?;
    Ok((selection, connected))
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

pub(crate) fn release_channel(prerelease: bool) -> ReleaseChannel {
    ReleaseChannel::from_prerelease_flag(prerelease)
}
