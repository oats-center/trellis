use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const DEFAULT_NATS_BOX_IMAGE: &str = "docker.io/natsio/nats-box:0.19.7";
const DEFAULT_OPERATOR_NAME: &str = "OATS Center";
const DEFAULT_SYSTEM_ACCOUNT: &str = "SYS";
const DEFAULT_AUTH_ACCOUNT: &str = "AUTH";
const DEFAULT_TRELLIS_ACCOUNT: &str = "TRELLIS";
const DEFAULT_SERVER_NAME: &str = "trellis-local";
const DEFAULT_TRELLIS_PORT: u16 = 3000;
const DEFAULT_NATS_SERVER_URL: &str = "nats://127.0.0.1:4222";
const DEFAULT_NATS_WEBSOCKET_URL: &str = "ws://localhost:8080";
const DEFAULT_PUBLIC_ORIGIN: &str = "http://localhost:3000";
const WORK_DIR: &str = "/work";
const MINIMUM_NSC_VERSION: NscVersion = NscVersion {
    major: 2,
    minor: 12,
    patch: 2,
};

/// Parsed `nsc --version` value.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NscVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl NscVersion {
    fn parse(output: &str) -> Option<Self> {
        let mut parts = output.split_whitespace();
        if parts.next()? != "nsc" || parts.next()? != "version" {
            return None;
        }

        let version = parts.next()?;
        if parts.next().is_some() {
            return None;
        }

        let mut version_parts = version.split('.');
        let major = version_parts.next()?.parse().ok()?;
        let minor = version_parts.next()?.parse().ok()?;
        let patch = version_parts.next()?.parse().ok()?;
        if version_parts.next().is_some() {
            return None;
        }

        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for NscVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Runtime used to execute the nats-box image for local NATS bootstrap generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerRuntime {
    /// Detect Podman first and then Docker.
    Auto,
    /// Use Podman and add SELinux mount relabeling.
    Podman,
    /// Use Docker without SELinux mount relabeling.
    Docker,
}

impl ContainerRuntime {
    fn command_name(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Podman => Some("podman"),
            Self::Docker => Some("docker"),
        }
    }
}

impl fmt::Display for ContainerRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Podman => "podman",
            Self::Docker => "docker",
        })
    }
}

/// Options for generating a local NATS bootstrap directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalNatsBootstrapOptions {
    /// Output directory for generated NATS config, credentials, and manifest files.
    pub out: PathBuf,
    /// Replace an existing non-empty output directory.
    pub force: bool,
    /// Container runtime selection for nats-box execution.
    pub container_runtime: ContainerRuntime,
    /// nats-box image used to run `nsc`.
    pub nats_box_image: String,
    /// NATS operator name.
    pub operator_name: String,
    /// System account name.
    pub system_account: String,
    /// Auth account name.
    pub auth_account: String,
    /// Trellis service account name.
    pub trellis_account: String,
    /// Local NATS server name written to `nats.conf`.
    pub server_name: String,
}

impl LocalNatsBootstrapOptions {
    /// Build options using the documented local bootstrap defaults.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> Self {
        Self {
            out: out.into(),
            force: false,
            container_runtime: ContainerRuntime::Auto,
            nats_box_image: DEFAULT_NATS_BOX_IMAGE.to_string(),
            operator_name: DEFAULT_OPERATOR_NAME.to_string(),
            system_account: DEFAULT_SYSTEM_ACCOUNT.to_string(),
            auth_account: DEFAULT_AUTH_ACCOUNT.to_string(),
            trellis_account: DEFAULT_TRELLIS_ACCOUNT.to_string(),
            server_name: DEFAULT_SERVER_NAME.to_string(),
        }
    }
}

/// Options for generating a complete local Trellis bootstrap bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTrellisBootstrapOptions {
    /// Output directory for generated NATS and Trellis bootstrap files.
    pub out: PathBuf,
    /// Replace an existing non-empty output directory.
    pub force: bool,
    /// Container runtime selection for nats-box execution.
    pub container_runtime: ContainerRuntime,
    /// nats-box image used to run `nsc`.
    pub nats_box_image: String,
    /// NATS operator name.
    pub operator_name: String,
    /// System account name.
    pub system_account: String,
    /// Auth account name.
    pub auth_account: String,
    /// Trellis service account name.
    pub trellis_account: String,
    /// Local NATS server name written to `nats.conf`.
    pub server_name: String,
    /// Trellis HTTP port written to `trellis/config.toml`.
    pub trellis_port: u16,
    /// Native NATS URL used by server-side Trellis services.
    pub nats_server_url: String,
    /// Browser-facing NATS websocket URL advertised to clients.
    pub nats_websocket_url: String,
    /// Public HTTP origin for OAuth redirects.
    pub public_origin: String,
}

impl LocalTrellisBootstrapOptions {
    /// Build options using the documented local Trellis bootstrap defaults.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> Self {
        Self {
            out: out.into(),
            force: false,
            container_runtime: ContainerRuntime::Auto,
            nats_box_image: DEFAULT_NATS_BOX_IMAGE.to_string(),
            operator_name: DEFAULT_OPERATOR_NAME.to_string(),
            system_account: DEFAULT_SYSTEM_ACCOUNT.to_string(),
            auth_account: DEFAULT_AUTH_ACCOUNT.to_string(),
            trellis_account: DEFAULT_TRELLIS_ACCOUNT.to_string(),
            server_name: DEFAULT_SERVER_NAME.to_string(),
            trellis_port: DEFAULT_TRELLIS_PORT,
            nats_server_url: DEFAULT_NATS_SERVER_URL.to_string(),
            nats_websocket_url: DEFAULT_NATS_WEBSOCKET_URL.to_string(),
            public_origin: DEFAULT_PUBLIC_ORIGIN.to_string(),
        }
    }
}

/// Paths and public keys generated by local NATS bootstrap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalNatsBootstrapManifest {
    /// Manifest schema version.
    pub version: u32,
    /// Container image used for generation.
    pub nats_box_image: String,
    /// Operator name passed to `nsc`.
    pub operator_name: String,
    /// Configured local NATS server name.
    pub server_name: String,
    /// Public account keys keyed by account role.
    pub accounts: BootstrapAccounts,
    /// Public user keys keyed by user role.
    pub users: BootstrapUsers,
    /// Relative paths to generated files.
    pub paths: BootstrapPaths,
}

/// Manifest for a complete local Trellis bootstrap bundle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTrellisBootstrapManifest {
    /// Manifest schema version.
    pub version: u32,
    /// Nested local NATS bootstrap manifest.
    pub nats: LocalNatsBootstrapManifest,
    /// Relative paths to generated Trellis bundle files.
    pub paths: LocalTrellisBootstrapPaths,
    /// URLs configured for local Trellis and NATS clients.
    pub urls: LocalTrellisBootstrapUrls,
}

/// Relative paths generated by full local Trellis bootstrap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTrellisBootstrapPaths {
    /// Nested local NATS manifest path.
    pub nats_manifest: String,
    /// Trellis service config path.
    pub trellis_config: String,
    /// Trellis session key seed file path.
    pub session_seed: String,
    /// Trellis service data directory path.
    pub trellis_data: String,
}

/// URLs configured by full local Trellis bootstrap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTrellisBootstrapUrls {
    /// Trellis public HTTP origin.
    pub public_origin: String,
    /// Native NATS server URL.
    pub nats_server: String,
    /// Browser-facing NATS websocket URL.
    pub nats_websocket: String,
    /// OAuth redirect base URL.
    pub oauth_redirect_base: String,
}

/// Public account keys generated by local NATS bootstrap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapAccounts {
    /// Public NKEY for the system account.
    pub system: PublicAccount,
    /// Public NKEY for the auth account.
    pub auth: PublicAccount,
    /// Public NKEY for the Trellis account.
    pub trellis: PublicAccount,
}

/// Named account public key metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAccount {
    /// Human-readable account name.
    pub name: String,
    /// Public account NKEY.
    pub public_key: String,
}

/// Public user keys generated by local NATS bootstrap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapUsers {
    /// Public NKEY for the dedicated system service user.
    pub system: PublicUser,
    /// Public NKEY for the auth service user in the auth account.
    pub auth_service: PublicUser,
    /// Public NKEY for the Trellis service user in the Trellis account.
    pub trellis_service: PublicUser,
}

/// Named user public key metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicUser {
    /// Human-readable user name.
    pub name: String,
    /// Public user NKEY.
    pub public_key: String,
}

/// Relative generated file paths.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapPaths {
    /// Local NATS server config path.
    pub nats_config: String,
    /// JWT resolver include path.
    pub jwt_config: String,
    /// Account JWT paths keyed by account role.
    pub account_jwts: BTreeMap<String, String>,
    /// Credential paths keyed by credential role.
    pub creds: BTreeMap<String, String>,
    /// Secret seed paths keyed by secret role.
    pub secrets: BTreeMap<String, String>,
    /// Auth callout environment file path.
    pub auth_callout_env: String,
}

/// Error returned while generating a local NATS bootstrap directory.
#[derive(Debug, Error)]
pub enum LocalBootstrapError {
    /// The output directory exists and contains files while force is disabled.
    #[error("output directory {path} is not empty; pass --force to replace it")]
    OutputDirectoryNotEmpty { path: PathBuf },
    /// No supported container runtime could be found.
    #[error("could not find podman or docker on PATH")]
    ContainerRuntimeNotFound,
    /// A process failed.
    #[error("{program} failed with status {status}: {stderr}")]
    CommandFailed {
        program: String,
        status: String,
        stderr: String,
    },
    /// The `nsc --version` container command failed.
    #[error("{program} failed checking nsc version in {image} with status {status}: {stderr}")]
    NscVersionCommandFailed {
        program: String,
        image: String,
        status: String,
        stderr: String,
    },
    /// The `nsc --version` output did not match the expected format.
    #[error("could not parse nsc version from {image}: {output}")]
    NscVersionUnparseable { image: String, output: String },
    /// The selected nats-box image contains an unsupported `nsc` version.
    #[error(
        "nsc version {actual} in {image} is unsupported; minimum required version is {minimum}"
    )]
    UnsupportedNscVersion {
        image: String,
        actual: NscVersion,
        minimum: NscVersion,
    },
    /// An expected value was missing from generated nsc metadata.
    #[error("missing generated {0}")]
    MissingGeneratedValue(&'static str),
    /// Filesystem I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Generated JSON could not be parsed or written.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Generate the local NATS bootstrap output directory.
pub fn generate_local_nats_bootstrap(
    options: &LocalNatsBootstrapOptions,
) -> Result<LocalNatsBootstrapManifest, LocalBootstrapError> {
    validate_output_dir(&options.out, options.force)?;
    let runtime = resolve_container_runtime(options.container_runtime)?;
    check_nsc_version(runtime, &options.nats_box_image)?;

    generate_local_nats_bootstrap_with_runtime(options, runtime)
}

fn generate_local_nats_bootstrap_with_runtime(
    options: &LocalNatsBootstrapOptions,
    runtime: ContainerRuntime,
) -> Result<LocalNatsBootstrapManifest, LocalBootstrapError> {
    if options.out.exists() && options.force {
        fs::remove_dir_all(&options.out)?;
    }
    create_layout(&options.out)?;
    write_static_configs(options)?;
    run_nsc_container(options, runtime)?;

    let generated = read_generated_metadata(&options.out)?;
    let manifest = build_manifest(options, &generated);
    write_manifest(&options.out, &manifest)?;
    remove_transient_files(&options.out)?;
    Ok(manifest)
}

/// Generate a complete local Trellis bootstrap bundle.
pub fn generate_local_trellis_bootstrap(
    options: &LocalTrellisBootstrapOptions,
) -> Result<LocalTrellisBootstrapManifest, LocalBootstrapError> {
    validate_output_dir(&options.out, options.force)?;
    let runtime = resolve_container_runtime(options.container_runtime)?;
    check_nsc_version(runtime, &options.nats_box_image)?;

    if options.out.exists() && options.force {
        fs::remove_dir_all(&options.out)?;
    }

    let nats_out = options.out.join("nats");
    let trellis_out = options.out.join("trellis");
    fs::create_dir_all(trellis_out.join("data"))?;

    let mut nats_options = LocalNatsBootstrapOptions::new(&nats_out);
    nats_options.force = false;
    nats_options.container_runtime = options.container_runtime;
    nats_options.nats_box_image = options.nats_box_image.clone();
    nats_options.operator_name = options.operator_name.clone();
    nats_options.system_account = options.system_account.clone();
    nats_options.auth_account = options.auth_account.clone();
    nats_options.trellis_account = options.trellis_account.clone();
    nats_options.server_name = options.server_name.clone();

    let nats_manifest = generate_local_nats_bootstrap_with_runtime(&nats_options, runtime)?;
    let authorization_directory = trellis_out.join("auth");
    generate_local_authorization_issuer(&authorization_directory)?;
    fs::write(
        trellis_out.join("config.toml"),
        render_trellis_config(options, &nats_manifest),
    )?;
    fs::write(
        trellis_out.join("session.seed"),
        format!("{}\n", generate_session_seed()),
    )?;
    let manifest = build_trellis_manifest(options, nats_manifest);
    fs::write(
        options.out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(manifest)
}

/// Validate whether an output directory can be used for generation.
pub fn validate_output_dir(out: &Path, force: bool) -> Result<(), LocalBootstrapError> {
    if !out.exists() || force {
        return Ok(());
    }

    if fs::read_dir(out)?.next().is_some() {
        return Err(LocalBootstrapError::OutputDirectoryNotEmpty {
            path: out.to_path_buf(),
        });
    }
    Ok(())
}

/// Render the local development NATS server config.
#[must_use]
pub fn render_nats_config(server_name: &str) -> String {
    format!(
        "server_name: {server_name}\n\nlisten: 0.0.0.0:4222\nhttp: 0.0.0.0:8222\n\nauthorization {{\n  timeout: \"10s\"\n}}\n\nwebsocket {{\n  listen: 0.0.0.0:8080\n  no_tls: true\n}}\n\njetstream {{\n  store_dir: /data\n}}\n\ninclude ./jwt.conf\n"
    )
}

/// Render the auth callout environment file without seed material in the manifest.
#[must_use]
pub fn render_auth_callout_env(generated: &GeneratedMetadata) -> String {
    format!(
        "AUTH_ACCOUNT={auth_account}\nAUTH_ACCOUNT_PUBLIC_KEY={auth_public}\nTRELLIS_ACCOUNT={trellis_account}\nTRELLIS_ACCOUNT_PUBLIC_KEY={trellis_public}\nAUTH_USER_PUBLIC_KEY={auth_user}\nTRELLIS_USER_PUBLIC_KEY={trellis_user}\nAUTH_ISSUER_SIGNING_SEED_FILE=./secrets/auth-issuer-signing.seed\nAUTH_TARGET_SIGNING_SEED_FILE=./secrets/auth-target-signing.seed\nAUTH_CALLOUT_XKEY_SEED_FILE=./secrets/auth-sx.seed\nAUTH_SERVICE_CREDS_FILE=./creds/auth-auth.creds\nTRELLIS_SERVICE_CREDS_FILE=./creds/trellis-auth.creds\n",
        auth_account = generated.auth_account_name,
        auth_public = generated.auth_account_public_key,
        trellis_account = generated.trellis_account_name,
        trellis_public = generated.trellis_account_public_key,
        auth_user = generated.auth_user_public_key,
        trellis_user = generated.trellis_user_public_key,
    )
}

/// Render the Rust Trellis runtime TOML config for a full local bootstrap bundle.
#[must_use]
pub fn render_trellis_config(
    options: &LocalTrellisBootstrapOptions,
    _nats_manifest: &LocalNatsBootstrapManifest,
) -> String {
    format!(
        r#"# Generated by `trellis local init` for local development.
instance_name = "Trellis"
event_session_seed_file = "./session.seed"
event_context_digest_file = "./data/session-context.digest"

[http]
port = {trellis_port}
public_origin = {public_origin}
origins = [{public_origin}]
allow_insecure_origins = [{public_origin}]
rate_limit_max = 60
rate_limit_window_ms = 60000

[nats]
servers = {nats_server_url}

[nats.runtime]
auth_creds_path = "../nats/creds/auth-auth.creds"
trellis_creds_path = "../nats/creds/trellis-auth.creds"
system_creds_path = "../nats/creds/system.creds"

[nats.auth_callout]
issuer_signing_seed_file = "../nats/secrets/auth-issuer-signing.seed"
target_signing_seed_file = "../nats/secrets/auth-target-signing.seed"
xkey_seed_file = "../nats/secrets/auth-sx.seed"

[auth.authorization]
issuer_signing_seed_file = "./auth/authorization-issuer.seed"
context_lifetime_seconds = 300
refresh_lead_seconds = 60
refresh_jitter_seconds = 15
minimum_context_lifetime_seconds = 76
maximum_bootstrap_jwt_lifetime_seconds = 3600
allowed_clock_skew_seconds = 30
maximum_context_bytes = 16384
maximum_permissions = 4096
context_bucket = "trellis_authorization_contexts"
registry_replicas = 1

[client]
nats_servers = [{nats_server_url}]
ws_nats_servers = [{nats_websocket_url}]

[leases]
bucket = "trellis_runtime_leases"
replicas = 1
ttl_ms = 30000
renew_ms = 5000

[auth.local_identity]
enabled = true
password_min_length = 12

[oauth]
redirect_base = {oauth_redirect_base}
always_show_provider_chooser = false

[platform.storage]
kind = "sqlite"
path = "./data/platform.sqlite"
journal_mode = "wal"
busy_timeout_ms = 5000
single_writer = true

[platform.ttl_ms]
sessions = 86400000
oauth = 300000
device_flow = 1800000
pending_auth = 300000
connections = 7200000
nats_jwt = 3600000

[jobs.storage]
kind = "sqlite"
path = "./data/jobs.sqlite"
journal_mode = "wal"
busy_timeout_ms = 5000
single_writer = true

[health]
transport_retention_hours = 1
transport_max_bytes = 16777216

[health.storage]
kind = "sqlite"
path = "./data/health.sqlite"
journal_mode = "wal"
busy_timeout_ms = 5000
single_writer = true

[events.storage]
kind = "sqlite"
path = "./data/events.sqlite"
journal_mode = "wal"
busy_timeout_ms = 5000
single_writer = true
"#,
        trellis_port = options.trellis_port,
        public_origin = json_string(&options.public_origin),
        nats_server_url = json_string(&options.nats_server_url),
        nats_websocket_url = json_string(&options.nats_websocket_url),
        oauth_redirect_base = json_string(&format!(
            "{}/auth/callback",
            trim_trailing_slashes(&options.public_origin)
        )),
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedMetadata {
    system_account_name: String,
    system_account_public_key: String,
    system_user_public_key: String,
    auth_account_name: String,
    auth_account_public_key: String,
    trellis_account_name: String,
    trellis_account_public_key: String,
    auth_user_public_key: String,
    trellis_user_public_key: String,
}

fn create_layout(out: &Path) -> Result<(), LocalBootstrapError> {
    fs::create_dir_all(out.join("data/jwt"))?;
    fs::create_dir_all(out.join("creds"))?;
    fs::create_dir_all(out.join("secrets"))?;
    fs::create_dir_all(out.join("generated"))?;
    Ok(())
}

fn write_static_configs(options: &LocalNatsBootstrapOptions) -> Result<(), LocalBootstrapError> {
    fs::write(
        options.out.join("nats.conf"),
        render_nats_config(&options.server_name),
    )?;
    fs::write(
        options.out.join("bootstrap-nsc.sh"),
        render_nsc_script(options),
    )?;
    Ok(())
}

fn run_nsc_container(
    options: &LocalNatsBootstrapOptions,
    runtime: ContainerRuntime,
) -> Result<(), LocalBootstrapError> {
    let mount = container_mount(&options.out, runtime);
    let program = container_runtime_program(runtime)?;
    let output = Command::new(program)
        .args(["run", "--rm", "-v"])
        .arg(mount)
        .args([&options.nats_box_image, "sh", "/work/bootstrap-nsc.sh"])
        .stdin(Stdio::null())
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    Err(LocalBootstrapError::CommandFailed {
        program: program.to_string(),
        status: output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn check_nsc_version(runtime: ContainerRuntime, image: &str) -> Result<(), LocalBootstrapError> {
    let program = container_runtime_program(runtime)?;
    let output = Command::new(program)
        .args(["run", "--rm", image, "nsc", "--version"])
        .stdin(Stdio::null())
        .output()?;

    if !output.status.success() {
        return Err(LocalBootstrapError::NscVersionCommandFailed {
            program: program.to_string(),
            image: image.to_string(),
            status: output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    check_nsc_version_output(image, String::from_utf8_lossy(&output.stdout).trim())
}

fn check_nsc_version_output(image: &str, output: &str) -> Result<(), LocalBootstrapError> {
    let actual =
        NscVersion::parse(output).ok_or_else(|| LocalBootstrapError::NscVersionUnparseable {
            image: image.to_string(),
            output: output.to_string(),
        })?;

    if actual < MINIMUM_NSC_VERSION {
        return Err(LocalBootstrapError::UnsupportedNscVersion {
            image: image.to_string(),
            actual,
            minimum: MINIMUM_NSC_VERSION,
        });
    }

    Ok(())
}

fn render_nsc_script(options: &LocalNatsBootstrapOptions) -> String {
    format!(
        r#"set -eu
OPERATOR_NAME={operator}
SYSTEM_ACCOUNT_NAME={system_account}
AUTH_ACCOUNT_NAME={auth_account}
TRELLIS_ACCOUNT_NAME={trellis_account}
export NSC_HOME=/work/.nsc
export NKEYS_PATH=/work/.nkeys
mkdir -p "$NSC_HOME" "$NKEYS_PATH" /work/data/jwt /work/creds /work/secrets /work/generated

nsc add operator --name "$OPERATOR_NAME" --sys
nsc add account --name "$AUTH_ACCOUNT_NAME"
nsc add account --name "$TRELLIS_ACCOUNT_NAME"
nsc edit account --name "$AUTH_ACCOUNT_NAME" --sk generate
nsc edit account --name "$TRELLIS_ACCOUNT_NAME" --sk generate
nsc edit account --name "$AUTH_ACCOUNT_NAME" --js-mem-storage -1 --js-disk-storage -1 --js-streams -1 --js-consumer 4096
nsc edit account --name "$TRELLIS_ACCOUNT_NAME" --js-mem-storage -1 --js-disk-storage -1 --js-streams -1 --js-consumer 4096

nsc add user --account "$SYSTEM_ACCOUNT_NAME" --name system --allow-pubsub ">"
nsc add user --account "$AUTH_ACCOUNT_NAME" --name auth --allow-pubsub ">"
nsc add user --account "$TRELLIS_ACCOUNT_NAME" --name auth --allow-pubsub ">"

AUTH_USER=$(nsc describe user --account "$AUTH_ACCOUNT_NAME" --name auth --field sub | tr -d '"')
TRELLIS_ACCOUNT=$(nsc describe account --name "$TRELLIS_ACCOUNT_NAME" --field sub | tr -d '"')
nsc edit authcallout --account "$AUTH_ACCOUNT_NAME" --auth-user "$AUTH_USER" --allowed-account "$TRELLIS_ACCOUNT" --curve generate

nsc generate creds --account "$AUTH_ACCOUNT_NAME" --name auth > /work/creds/auth-auth.creds
nsc generate creds --account "$TRELLIS_ACCOUNT_NAME" --name auth > /work/creds/trellis-auth.creds
nsc generate creds --account "$SYSTEM_ACCOUNT_NAME" --name system > /work/creds/system.creds

SYS_ACCOUNT=$(nsc describe account --name "$SYSTEM_ACCOUNT_NAME" --field sub | tr -d '"')
AUTH_ACCOUNT=$(nsc describe account --name "$AUTH_ACCOUNT_NAME" --field sub | tr -d '"')
TRELLIS_ACCOUNT=$(nsc describe account --name "$TRELLIS_ACCOUNT_NAME" --field sub | tr -d '"')
SYSTEM_USER=$(nsc describe user --account "$SYSTEM_ACCOUNT_NAME" --name system --field sub | tr -d '"')
AUTH_USER=$(nsc describe user --account "$AUTH_ACCOUNT_NAME" --name auth --field sub | tr -d '"')
TRELLIS_USER=$(nsc describe user --account "$TRELLIS_ACCOUNT_NAME" --name auth --field sub | tr -d '"')

nsc describe account --name "$SYSTEM_ACCOUNT_NAME" --raw > "/work/data/jwt/${{SYS_ACCOUNT}}.jwt"
nsc describe account --name "$AUTH_ACCOUNT_NAME" --raw > "/work/data/jwt/${{AUTH_ACCOUNT}}.jwt"
nsc describe account --name "$TRELLIS_ACCOUNT_NAME" --raw > "/work/data/jwt/${{TRELLIS_ACCOUNT}}.jwt"
nsc generate config --nats-resolver --config-file /work/generated/jwt.conf --force --sys-account "$SYSTEM_ACCOUNT_NAME"

nsc list keys --account "$AUTH_ACCOUNT_NAME" --accounts --show-seeds --json > /work/generated/auth-keys.json
nsc list keys --account "$TRELLIS_ACCOUNT_NAME" --accounts --show-seeds --json > /work/generated/trellis-keys.json
nsc list keys --show-seeds --json > /work/generated/all-keys.json

cat > /work/generated/metadata.json <<EOF
{{
  "systemAccountName": "${{SYSTEM_ACCOUNT_NAME}}",
  "systemAccountPublicKey": "${{SYS_ACCOUNT}}",
  "systemUserPublicKey": "${{SYSTEM_USER}}",
  "authAccountName": "${{AUTH_ACCOUNT_NAME}}",
  "authAccountPublicKey": "${{AUTH_ACCOUNT}}",
  "trellisAccountName": "${{TRELLIS_ACCOUNT_NAME}}",
  "trellisAccountPublicKey": "${{TRELLIS_ACCOUNT}}",
  "authUserPublicKey": "${{AUTH_USER}}",
  "trellisUserPublicKey": "${{TRELLIS_USER}}"
}}
EOF
"#,
        operator = shell_quote(&options.operator_name),
        system_account = shell_quote(&options.system_account),
        auth_account = shell_quote(&options.auth_account),
        trellis_account = shell_quote(&options.trellis_account),
    )
}

fn read_generated_metadata(out: &Path) -> Result<GeneratedMetadata, LocalBootstrapError> {
    let metadata = serde_json::from_slice::<GeneratedMetadata>(&fs::read(
        out.join("generated/metadata.json"),
    )?)?;
    fs::write(
        out.join("auth-callout.env"),
        render_auth_callout_env(&metadata),
    )?;
    write_generated_seeds(out)?;
    normalize_jwt_config(out)?;
    Ok(metadata)
}

fn write_generated_seeds(out: &Path) -> Result<(), LocalBootstrapError> {
    let auth_keys = read_json_file(&out.join("generated/auth-keys.json"))?;
    let trellis_keys = read_json_file(&out.join("generated/trellis-keys.json"))?;
    fs::write(
        out.join("secrets/auth-issuer-signing.seed"),
        first_seed_matching(
            &auth_keys,
            "SA",
            Some(true),
            Some(false),
            "account signing key seed",
        )?,
    )?;
    fs::write(
        out.join("secrets/auth-target-signing.seed"),
        first_seed_matching(
            &trellis_keys,
            "SA",
            Some(true),
            Some(false),
            "account signing key seed",
        )?,
    )?;
    fs::write(
        out.join("secrets/auth-sx.seed"),
        first_seed_matching(
            &auth_keys,
            "SX",
            Some(false),
            Some(true),
            "auth callout xkey seed",
        )?,
    )?;
    Ok(())
}

fn normalize_jwt_config(out: &Path) -> Result<(), LocalBootstrapError> {
    let generated = fs::read_to_string(out.join("generated/jwt.conf"))?;
    let mut normalized = generated.replace(WORK_DIR, "/data");
    normalized = replace_resolver_dir(&normalized);
    fs::write(out.join("jwt.conf"), normalized)?;
    Ok(())
}

fn replace_resolver_dir(config: &str) -> String {
    config
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("dir:") || trimmed.starts_with("dir ") {
                let indent_len = line.len() - trimmed.len();
                format!("{}dir: /data/jwt", &line[..indent_len])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn build_manifest(
    options: &LocalNatsBootstrapOptions,
    generated: &GeneratedMetadata,
) -> LocalNatsBootstrapManifest {
    LocalNatsBootstrapManifest {
        version: 1,
        nats_box_image: options.nats_box_image.clone(),
        operator_name: options.operator_name.clone(),
        server_name: options.server_name.clone(),
        accounts: BootstrapAccounts {
            system: PublicAccount {
                name: generated.system_account_name.clone(),
                public_key: generated.system_account_public_key.clone(),
            },
            auth: PublicAccount {
                name: generated.auth_account_name.clone(),
                public_key: generated.auth_account_public_key.clone(),
            },
            trellis: PublicAccount {
                name: generated.trellis_account_name.clone(),
                public_key: generated.trellis_account_public_key.clone(),
            },
        },
        users: BootstrapUsers {
            system: PublicUser {
                name: "system".to_string(),
                public_key: generated.system_user_public_key.clone(),
            },
            auth_service: PublicUser {
                name: "auth".to_string(),
                public_key: generated.auth_user_public_key.clone(),
            },
            trellis_service: PublicUser {
                name: "auth".to_string(),
                public_key: generated.trellis_user_public_key.clone(),
            },
        },
        paths: BootstrapPaths {
            nats_config: "nats.conf".to_string(),
            jwt_config: "jwt.conf".to_string(),
            account_jwts: BTreeMap::from([
                (
                    "system".to_string(),
                    format!("data/jwt/{}.jwt", generated.system_account_public_key),
                ),
                (
                    "auth".to_string(),
                    format!("data/jwt/{}.jwt", generated.auth_account_public_key),
                ),
                (
                    "trellis".to_string(),
                    format!("data/jwt/{}.jwt", generated.trellis_account_public_key),
                ),
            ]),
            creds: BTreeMap::from([
                (
                    "systemService".to_string(),
                    "creds/system.creds".to_string(),
                ),
                (
                    "authService".to_string(),
                    "creds/auth-auth.creds".to_string(),
                ),
                (
                    "trellisService".to_string(),
                    "creds/trellis-auth.creds".to_string(),
                ),
            ]),
            secrets: BTreeMap::from([
                (
                    "authIssuerSigning".to_string(),
                    "secrets/auth-issuer-signing.seed".to_string(),
                ),
                (
                    "authTargetSigning".to_string(),
                    "secrets/auth-target-signing.seed".to_string(),
                ),
                (
                    "authCalloutXKey".to_string(),
                    "secrets/auth-sx.seed".to_string(),
                ),
            ]),
            auth_callout_env: "auth-callout.env".to_string(),
        },
    }
}

fn build_trellis_manifest(
    options: &LocalTrellisBootstrapOptions,
    nats: LocalNatsBootstrapManifest,
) -> LocalTrellisBootstrapManifest {
    LocalTrellisBootstrapManifest {
        version: 1,
        nats,
        paths: LocalTrellisBootstrapPaths {
            nats_manifest: "nats/manifest.json".to_string(),
            trellis_config: "trellis/config.toml".to_string(),
            session_seed: "trellis/session.seed".to_string(),
            trellis_data: "trellis/data".to_string(),
        },
        urls: LocalTrellisBootstrapUrls {
            public_origin: options.public_origin.clone(),
            nats_server: options.nats_server_url.clone(),
            nats_websocket: options.nats_websocket_url.clone(),
            oauth_redirect_base: format!(
                "{}/auth/callback",
                trim_trailing_slashes(&options.public_origin)
            ),
        },
    }
}

fn write_manifest(
    out: &Path,
    manifest: &LocalNatsBootstrapManifest,
) -> Result<(), LocalBootstrapError> {
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(manifest)? + "\n",
    )?;
    Ok(())
}

fn remove_transient_files(out: &Path) -> Result<(), LocalBootstrapError> {
    let script = out.join("bootstrap-nsc.sh");
    if script.exists() {
        fs::remove_file(script)?;
    }
    let generated = out.join("generated");
    if generated.exists() {
        fs::remove_dir_all(generated)?;
    }
    let nsc = out.join(".nsc");
    if nsc.exists() {
        fs::remove_dir_all(nsc)?;
    }
    let nkeys = out.join(".nkeys");
    if nkeys.exists() {
        fs::remove_dir_all(nkeys)?;
    }
    Ok(())
}

fn resolve_container_runtime(
    requested: ContainerRuntime,
) -> Result<ContainerRuntime, LocalBootstrapError> {
    match requested {
        ContainerRuntime::Podman | ContainerRuntime::Docker => Ok(requested),
        ContainerRuntime::Auto if command_exists("podman") => Ok(ContainerRuntime::Podman),
        ContainerRuntime::Auto if command_exists("docker") => Ok(ContainerRuntime::Docker),
        ContainerRuntime::Auto => Err(LocalBootstrapError::ContainerRuntimeNotFound),
    }
}

fn command_exists(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn container_runtime_program(
    runtime: ContainerRuntime,
) -> Result<&'static str, LocalBootstrapError> {
    runtime
        .command_name()
        .ok_or(LocalBootstrapError::ContainerRuntimeNotFound)
}

fn container_mount(out: &Path, runtime: ContainerRuntime) -> OsString {
    let suffix = if runtime == ContainerRuntime::Podman {
        ":/work:Z"
    } else {
        ":/work"
    };
    let mut mount = out.as_os_str().to_os_string();
    mount.push(suffix);
    mount
}

fn read_json_file(path: &Path) -> Result<Value, LocalBootstrapError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn first_seed_matching(
    value: &Value,
    prefix: &str,
    signing: Option<bool>,
    curve: Option<bool>,
    missing_label: &'static str,
) -> Result<String, LocalBootstrapError> {
    let mut seeds = Vec::new();
    collect_matching_seed_strings(value, prefix, signing, curve, &mut seeds);
    seeds
        .into_iter()
        .next()
        .ok_or(LocalBootstrapError::MissingGeneratedValue(missing_label))
}

fn collect_matching_seed_strings(
    value: &Value,
    prefix: &str,
    signing: Option<bool>,
    curve: Option<bool>,
    seeds: &mut Vec<String>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_matching_seed_strings(item, prefix, signing, curve, seeds);
            }
        }
        Value::Object(map) => {
            if let Some(seed) = map.get("seed").and_then(Value::as_str) {
                let signing_matches = signing
                    .map(|expected| map.get("signing").and_then(Value::as_bool) == Some(expected))
                    .unwrap_or(true);
                let curve_matches = curve
                    .map(|expected| map.get("curve").and_then(Value::as_bool) == Some(expected))
                    .unwrap_or(true);
                if seed.starts_with(prefix) && signing_matches && curve_matches {
                    seeds.push(seed.to_string());
                }
            }

            for item in map.values() {
                collect_matching_seed_strings(item, prefix, signing, curve, seeds);
            }
        }
        _ => {}
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing string should not fail")
}

/// Generate a fresh base64url-encoded 32-byte session seed for the event log.
///
/// Shared with `trellis-bootstrap` so both bootstrap generators write byte-identical seeds.
#[must_use]
pub fn generate_session_seed() -> String {
    let seed: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(seed)
}

/// Generate a private online authorization-context issuer seed without trust files.
pub fn generate_local_authorization_issuer(directory: &Path) -> Result<(), LocalBootstrapError> {
    fs::create_dir_all(directory)?;
    write_secret(
        &directory.join("authorization-issuer.seed"),
        &generate_session_seed(),
    )
}

fn write_secret(path: &Path, value: &str) -> Result<(), LocalBootstrapError> {
    use std::io::Write as _;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    writeln!(options.open(path)?, "{value}")?;
    Ok(())
}

fn trim_trailing_slashes(value: &str) -> &str {
    value.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_output_dir_rejects_non_empty_without_force() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("existing"), "x").expect("write file");

        let error = validate_output_dir(temp.path(), false).expect_err("should reject");

        assert!(matches!(
            error,
            LocalBootstrapError::OutputDirectoryNotEmpty { .. }
        ));
    }

    #[test]
    fn validate_output_dir_accepts_non_empty_with_force() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("existing"), "x").expect("write file");

        validate_output_dir(temp.path(), true).expect("force should allow non-empty dir");
    }

    #[test]
    fn manifest_omits_seed_values() {
        let options = LocalNatsBootstrapOptions::new("/tmp/trellis-local");
        let generated = generated_metadata();
        let manifest = build_manifest(&options, &generated);
        let json = serde_json::to_string(&manifest).expect("manifest json");

        assert!(json.contains("auth-issuer-signing.seed"));
        assert!(json.contains("system.creds"));
        assert!(json.contains("UDYSYSTEM"));
        assert!(!json.contains("SA"));
        assert!(!json.contains("SX"));
    }

    #[test]
    fn nats_manifest_includes_system_service_metadata() {
        let options = LocalNatsBootstrapOptions::new("/tmp/trellis-local");
        let generated = generated_metadata();
        let manifest = build_manifest(&options, &generated);

        assert_eq!(manifest.users.system.name, "system");
        assert_eq!(manifest.users.system.public_key, "UDYSYSTEM");
        assert_eq!(
            manifest.paths.creds.get("systemService"),
            Some(&"creds/system.creds".to_string())
        );
    }

    #[test]
    fn nsc_script_generates_system_service_credentials() {
        let options = LocalNatsBootstrapOptions::new("/tmp/trellis-local");
        let script = render_nsc_script(&options);

        assert!(script.contains(
            "nsc add user --account \"$SYSTEM_ACCOUNT_NAME\" --name system --allow-pubsub \">\""
        ));
        assert!(script.contains(
            "nsc generate creds --account \"$SYSTEM_ACCOUNT_NAME\" --name system > /work/creds/system.creds"
        ));
        assert!(script.contains(
            "SYSTEM_USER=$(nsc describe user --account \"$SYSTEM_ACCOUNT_NAME\" --name system --field sub"
        ));
        assert!(script.contains("\"systemUserPublicKey\": \"${SYSTEM_USER}\""));
    }

    #[test]
    fn nsc_version_parse_accepts_expected_output() {
        assert_eq!(
            NscVersion::parse("nsc version 2.12.2"),
            Some(NscVersion {
                major: 2,
                minor: 12,
                patch: 2,
            })
        );
        assert_eq!(
            NscVersion::parse("nsc version 2.13.0\n"),
            Some(NscVersion {
                major: 2,
                minor: 13,
                patch: 0,
            })
        );
    }

    #[test]
    fn nsc_version_parse_rejects_unexpected_output() {
        assert_eq!(NscVersion::parse("2.12.2"), None);
        assert_eq!(NscVersion::parse("nsc 2.12.2"), None);
        assert_eq!(NscVersion::parse("nsc version 2.12"), None);
        assert_eq!(NscVersion::parse("nsc version 2.12.2 extra"), None);
    }

    #[test]
    fn nsc_version_check_accepts_minimum_and_newer_versions() {
        check_nsc_version_output("nats-box:test", "nsc version 2.12.2")
            .expect("minimum version should pass");
        check_nsc_version_output("nats-box:test", "nsc version 2.13.0")
            .expect("newer version should pass");
    }

    #[test]
    fn nsc_version_check_rejects_older_version_with_clear_error() {
        let error = check_nsc_version_output("nats-box:test", "nsc version 2.12.1")
            .expect_err("older version should fail");

        assert_eq!(
            error.to_string(),
            "nsc version 2.12.1 in nats-box:test is unsupported; minimum required version is 2.12.2"
        );
    }

    #[test]
    fn nsc_version_check_rejects_unparseable_output_with_image_name() {
        let error = check_nsc_version_output("nats-box:test", "nsc 2.12.2")
            .expect_err("unparseable version should fail");

        assert_eq!(
            error.to_string(),
            "could not parse nsc version from nats-box:test: nsc 2.12.2"
        );
    }

    #[test]
    fn trellis_config_uses_expected_local_paths_and_urls() {
        let mut options = LocalTrellisBootstrapOptions::new("/tmp/trellis-local");
        options.trellis_port = 4242;
        options.nats_server_url = "nats://nats.example.test:4222".to_string();
        options.nats_websocket_url = "wss://nats.example.test/ws".to_string();
        options.public_origin = "https://trellis.example.test/".to_string();
        let nats_manifest = nats_manifest();

        let config = render_trellis_config(&options, &nats_manifest);

        assert!(config.contains("port = 4242"));
        for path in [
            "./data/platform.sqlite",
            "./data/jobs.sqlite",
            "./data/health.sqlite",
            "./data/events.sqlite",
        ] {
            assert!(config.contains(&format!("path = \"{path}\"")));
        }
        assert!(config.contains("[auth.local_identity]"));
        assert!(config.contains("enabled = true"));
        assert!(config.contains("servers = \"nats://nats.example.test:4222\""));
        assert!(config.contains("system_creds_path = \"../nats/creds/system.creds\""));
        assert!(config.contains("trellis_creds_path = \"../nats/creds/trellis-auth.creds\""));
        assert!(config.contains("auth_creds_path = \"../nats/creds/auth-auth.creds\""));
        assert!(config
            .contains("issuer_signing_seed_file = \"../nats/secrets/auth-issuer-signing.seed\""));
        assert!(config
            .contains("target_signing_seed_file = \"../nats/secrets/auth-target-signing.seed\""));
        assert!(config.contains("xkey_seed_file = \"../nats/secrets/auth-sx.seed\""));
        assert!(config.contains("event_session_seed_file = \"./session.seed\""));
        assert!(config.contains("issuer_signing_seed_file = \"./auth/authorization-issuer.seed\""));
        assert!(!config.contains("trust_root_file"));
        assert!(!config.contains("issuer_manifest_file"));
        assert!(!config.contains("trust_bucket"));
        assert!(config.contains("event_context_digest_file = \"./data/session-context.digest\""));
        assert!(config.contains("ws_nats_servers = [\"wss://nats.example.test/ws\"]"));
        assert!(config.contains("nats_servers = [\"nats://nats.example.test:4222\"]"));
        assert!(config.contains("redirect_base = \"https://trellis.example.test/auth/callback\""));
        assert!(!config.contains("github"));
        assert!(!config.contains("clientSecretFile"));
    }

    #[test]
    fn trellis_manifest_omits_secret_values() {
        let options = LocalTrellisBootstrapOptions::new("/tmp/trellis-local");
        let manifest = build_trellis_manifest(&options, nats_manifest());
        let json = serde_json::to_string(&manifest).expect("manifest json");

        assert!(json.contains("session.seed"));
        assert!(!json.contains("local-dev-session-seed"));
        assert!(!json.contains("SA"));
        assert!(!json.contains("SX"));
    }

    #[test]
    fn trellis_options_use_documented_defaults() {
        let options = LocalTrellisBootstrapOptions::new("./local");

        assert_eq!(options.out, PathBuf::from("./local"));
        assert!(!options.force);
        assert_eq!(options.container_runtime, ContainerRuntime::Auto);
        assert_eq!(options.nats_box_image, DEFAULT_NATS_BOX_IMAGE);
        assert_eq!(options.operator_name, DEFAULT_OPERATOR_NAME);
        assert_eq!(options.system_account, DEFAULT_SYSTEM_ACCOUNT);
        assert_eq!(options.auth_account, DEFAULT_AUTH_ACCOUNT);
        assert_eq!(options.trellis_account, DEFAULT_TRELLIS_ACCOUNT);
        assert_eq!(options.server_name, DEFAULT_SERVER_NAME);
        assert_eq!(options.trellis_port, DEFAULT_TRELLIS_PORT);
        assert_eq!(options.nats_server_url, DEFAULT_NATS_SERVER_URL);
        assert_eq!(options.nats_websocket_url, DEFAULT_NATS_WEBSOCKET_URL);
        assert_eq!(options.public_origin, DEFAULT_PUBLIC_ORIGIN);
    }

    #[test]
    fn generated_session_seed_is_base64url_encoded_32_bytes() {
        let seed = generate_session_seed();
        let decoded = URL_SAFE_NO_PAD.decode(seed).expect("decode session seed");

        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn local_authorization_issuer_is_private_and_never_overwritten() {
        let directory = tempfile::tempdir().expect("temp directory");
        generate_local_authorization_issuer(directory.path()).expect("generate issuer");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("read trust directory")
                .map(|entry| entry.expect("read trust entry").file_name())
                .collect::<std::collections::BTreeSet<_>>(),
            ["authorization-issuer.seed"]
                .into_iter()
                .map(OsString::from)
                .collect()
        );
        let path = directory.path().join("authorization-issuer.seed");
        let seed = fs::read_to_string(&path).expect("read issuer seed");
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(seed.trim())
                .expect("decode seed")
                .len(),
            32
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path)
                    .expect("seed metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(generate_local_authorization_issuer(directory.path()).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read retained seed"), seed);
    }

    #[test]
    fn nats_config_uses_local_defaults() {
        let config = render_nats_config("trellis-local");

        assert!(config.contains("server_name: trellis-local"));
        assert!(config.contains("listen: 0.0.0.0:4222"));
        assert!(config.contains("http: 0.0.0.0:8222"));
        assert!(config.contains("timeout: \"10s\""));
        assert!(config.contains("listen: 0.0.0.0:8080"));
        assert!(config.contains("no_tls: true"));
        assert!(config.contains("store_dir: /data"));
        assert!(config.contains("include ./jwt.conf"));
    }

    #[test]
    fn jwt_config_normalization_forces_container_resolver_dir() {
        let config = "resolver {\n  dir: /work/jwt/accounts\n}\n";

        assert_eq!(
            replace_resolver_dir(config),
            "resolver {\n  dir: /data/jwt\n}\n"
        );
    }

    #[test]
    fn first_seed_matching_uses_key_metadata_flags() {
        let keys = serde_json::json!([
            {
                "seed": "SAA_PRIMARY",
                "signing": false,
                "curve": false
            },
            {
                "seed": "SAA_SIGNING",
                "signing": true,
                "curve": false
            },
            {
                "seed": "SXA_CURVE",
                "signing": false,
                "curve": true
            }
        ]);

        assert_eq!(
            first_seed_matching(&keys, "SA", Some(true), Some(false), "signing")
                .expect("signing seed"),
            "SAA_SIGNING"
        );
        assert_eq!(
            first_seed_matching(&keys, "SX", Some(false), Some(true), "curve").expect("curve seed"),
            "SXA_CURVE"
        );
    }

    fn generated_metadata() -> GeneratedMetadata {
        GeneratedMetadata {
            system_account_name: "SYS".to_string(),
            system_account_public_key: "ADYSYS".to_string(),
            system_user_public_key: "UDYSYSTEM".to_string(),
            auth_account_name: "AUTH".to_string(),
            auth_account_public_key: "ADYAUTH".to_string(),
            trellis_account_name: "TRELLIS".to_string(),
            trellis_account_public_key: "ADYTRELLIS".to_string(),
            auth_user_public_key: "UDYAUTH".to_string(),
            trellis_user_public_key: "UDYTRELLIS".to_string(),
        }
    }

    fn nats_manifest() -> LocalNatsBootstrapManifest {
        build_manifest(
            &LocalNatsBootstrapOptions::new("/tmp/trellis-local/nats"),
            &generated_metadata(),
        )
    }
}
