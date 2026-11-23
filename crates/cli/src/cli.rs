use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

mod auth;
mod deploy;
mod events;
mod resources;
mod self_cmd;

pub use auth::*;
pub use deploy::*;
pub use events::*;
pub use resources::*;
pub use self_cmd::*;

#[derive(Debug, Parser)]
#[command(name = "trellis", version = env!("TRELLIS_BUILD_VERSION"), about = "Trellis CLI")]
/// Top-level Trellis CLI arguments shared by all subcommands.
pub struct Cli {
    #[arg(long, global = true, default_value = "text")]
    /// Render command output as human-readable text or machine-readable JSON.
    pub format: OutputFormat,

    #[arg(long, global = true)]
    /// Never ask questions: fail, naming the flags to give, when a command
    /// needs input it was not given. Questions are also off without a terminal.
    pub no_input: bool,

    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    /// Increase log verbosity. Repeat for additional detail.
    pub verbose: u8,

    #[command(subcommand)]
    pub command: TopLevelCommand,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
/// Output encoder used for human-facing commands.
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
/// Root command tree for Trellis operator, admin, and local development tasks.
pub enum TopLevelCommand {
    /// Add or replace one local API dependency, lock it, and install.
    Add(AddArgs),
    /// Remove one API dependency and reconcile installed outputs.
    Rm(RmArgs),
    /// Parse, resolve, and validate the local source package.
    Check(ProjectRootArgs),
    /// Refresh local package dependencies, lock them, and install.
    Update(UpdateArgs),
    /// Recreate the local generated package from the exact lock.
    Install(ProjectRootArgs),
    /// Generate the project package from Trellis IDL.
    Generate(GenerateArgs),
    /// Publish every project-owned canonical API to OCI.
    Publish(PublishArgs),
    /// Authorize the native CLI through the selected server's device flow.
    Login(LoginArgs),
    /// Revoke the current login and clear the selected local credential.
    Logout,
    /// Show the authenticated principal and login.
    Whoami,
    /// Manage authorization signing issuers.
    Issuers(IssuersCommand),
    /// Manage Trellis users.
    Users(UsersCommand),
    /// Manage roles, including the roles new users get.
    Roles(RolesCommand),
    /// Register public OAuth clients, including loopback development clients.
    #[command(subcommand)]
    Clients(ClientsCommand),
    /// Review and accept API catalog definitions.
    #[command(subcommand)]
    Apis(ApisCommand),
    /// Brand the login Portal for the installation or one app.
    #[command(subcommand)]
    Theme(ThemeCommand),
    /// Choose how users sign in to this Trellis.
    #[command(subcommand)]
    SignIn(SignInCommand),
    /// Manage service deployments.
    Svc(SvcCommand),
    /// Manage device deployments.
    Dev(DevCommand),
    /// Show or change the installation's rules for enrolled and offline
    /// service instances.
    #[command(subcommand)]
    InstancePolicy(InstancePolicyCommand),
    /// Add and remove the registries Trellis pulls published packages from.
    #[command(subcommand)]
    Registries(RegistriesCommand),
    /// Inspect and destroy provisioned resources.
    #[command(subcommand)]
    Resources(ResourcesCommand),
    /// Query and administer Events.
    #[command(subcommand)]
    Events(EventsCommand),
    /// Show the disk each kind of Trellis data takes, and how close
    /// JetStream is to its storage and memory limits.
    Storage,
    /// Report the state of this Trellis installation in one view: the server,
    /// health, JetStream, disk, certificates, recent failures, and stuck work.
    /// Exits non-zero when anything needs attention.
    Doctor,
    /// Generate or derive Trellis keys.
    Keys(KeysCommand),
    /// Check for or install CLI updates.
    Upgrade(UpgradeCommand),
    /// Print the current CLI version.
    Version,
    /// Generate shell completion scripts for Trellis.
    Completion { shell: Shell },
}

impl TopLevelCommand {
    /// Catalog command label for the CLI duration metric, if instrumented.
    ///
    /// Completion and version stay offline and never start exporters.
    pub fn telemetry_label(&self) -> Option<&'static str> {
        Some(match self {
            Self::Add(_) => "add",
            Self::Rm(_) => "rm",
            Self::Check(_) => "check",
            Self::Update(_) => "update",
            Self::Install(_) => "install",
            Self::Generate(_) => "generate",
            Self::Publish(_) => "publish",
            Self::Login(_) => "login",
            Self::Logout => "logout",
            Self::Whoami => "whoami",
            Self::Issuers(_) => "issuers",
            Self::Users(_) => "users",
            Self::Roles(_) => "roles",
            Self::Clients(_) => "clients",
            Self::Apis(_) => "apis",
            Self::Theme(_) => "theme",
            Self::SignIn(_) => "sign-in",
            Self::Svc(_) => "svc",
            Self::Dev(_) => "dev",
            Self::InstancePolicy(_) => "instance-policy",
            Self::Registries(_) => "registries",
            Self::Resources(_) => "resources",
            Self::Events(_) => "events",
            Self::Storage => "storage",
            Self::Doctor => "doctor",
            Self::Keys(_) => "keys",
            Self::Upgrade(_) => "upgrade",
            Self::Version | Self::Completion { .. } => return None,
        })
    }
}

#[derive(Debug, clap::Args)]
/// Shared project-root selection for local package commands.
pub struct ProjectRootArgs {
    #[arg(long, default_value = ".")]
    /// Directory containing `trellis.toml`.
    pub root: PathBuf,
}

#[derive(Debug, clap::Args)]
/// Select dependencies to refresh in a local Trellis project.
pub struct UpdateArgs {
    /// Dependency alias to refresh; omit to refresh every dependency.
    pub dependency_alias: Option<String>,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Generate the project package once or whenever IDL sources change.
pub struct GenerateArgs {
    #[arg(short = 'w', long)]
    /// Watch the project and direct local dependencies for source changes.
    pub watch: bool,
    #[arg(long, conflicts_with = "watch")]
    /// Check for missing, stale, or extra generated files without changing outputs.
    pub check: bool,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Add one local Trellis project or published package.
pub struct AddArgs {
    /// Relative Trellis project path, or a published package's full identity
    /// such as `ghcr.io/acme/orders`, which says where it is pulled from.
    pub source: String,
    #[arg(long)]
    /// Semantic Version requirement; defaults to a caret of the exact release.
    pub version: Option<String>,
    #[arg(long)]
    /// Dependency alias imports name it by; defaults to the package name with
    /// `-` as `_`. A second package with the same name, from another
    /// workspace, needs its own.
    pub alias: Option<String>,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Publish the package to its workspace, `<workspace>/<name>`.
pub struct PublishArgs {
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Remove one dependency by its alias, package name, or full identity.
pub struct RmArgs {
    /// Dependency alias, package name, or full identity such as
    /// `ghcr.io/acme/orders`.
    pub api_id: String,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Generate a Trellis keypair, optionally from a fixed seed.
pub struct KeygenArgs {
    #[arg(long, allow_hyphen_values = true)]
    /// Reuse an existing base64url-encoded 32-byte Ed25519 seed.
    pub seed: Option<String>,

    #[arg(long)]
    /// Write the generated private seed to this file.
    pub out: Option<PathBuf>,

    #[arg(long)]
    /// Write the derived public key to this file.
    pub pubout: Option<PathBuf>,
}

#[cfg(test)]
mod tests;
