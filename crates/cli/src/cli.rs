use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

mod auth;
mod bootstrap;
mod deploy;
mod events;
mod resources;
mod self_cmd;

pub use auth::*;
#[cfg(feature = "runtime")]
pub use bootstrap::*;
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
    /// Start a detached portal login against a Trellis auth service.
    Login(LoginArgs),
    /// Revoke the current admin session and clear local session state.
    Logout,
    /// Show the currently logged-in Trellis admin session.
    Whoami,
    /// Manage participant-scoped identity grants.
    Identity(IdentityCommand),
    /// Install participant definitions without granting authority.
    Participants(ParticipantsCommand),
    /// Manage authorization signing issuers.
    Issuers(IssuersCommand),
    /// Manage Trellis users.
    Users(UsersCommand),
    /// Inspect and manage login portals.
    Portals(PortalsCommand),
    /// Manage service deployments.
    Svc(SvcCommand),
    /// Manage device deployments.
    Dev(DevCommand),
    /// Inspect and destroy provisioned resources.
    #[command(subcommand)]
    Resources(ResourcesCommand),
    /// Query and administer Events.
    #[command(subcommand)]
    Events(EventsCommand),
    /// Run one-time initialization workflows.
    #[cfg(feature = "runtime")]
    Init(InitCommand),
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
            Self::Identity(_) => "identity",
            Self::Participants(_) => "participants",
            Self::Issuers(_) => "issuers",
            Self::Users(_) => "users",
            Self::Portals(_) => "portals",
            Self::Svc(_) => "svc",
            Self::Dev(_) => "dev",
            Self::Resources(_) => "resources",
            Self::Events(_) => "events",
            #[cfg(feature = "runtime")]
            Self::Init(_) => "init",
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
/// Add one local Trellis project or remote API ID.
pub struct AddArgs {
    /// Relative Trellis project path or stable API ID such as `acme.orders@v1`.
    pub source: String,
    #[arg(long)]
    /// Semantic Version requirement; defaults to a caret of the exact release.
    pub version: Option<String>,
    #[arg(long)]
    /// Named OCI registry for a remote API.
    pub registry: Option<String>,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Publish project-owned canonical APIs to OCI.
pub struct PublishArgs {
    #[arg(long)]
    /// Named OCI registry; defaults to `default-registry`.
    pub registry: Option<String>,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Remove one API dependency by stable API ID.
pub struct RmArgs {
    /// Stable API ID such as `acme.orders@v1`.
    pub api_id: String,
    #[command(flatten)]
    pub project: ProjectRootArgs,
}

#[derive(Debug, clap::Args)]
/// Generate a Trellis keypair, optionally from a fixed seed.
pub struct KeygenArgs {
    #[arg(long)]
    /// Reuse an existing base64url-encoded 32-byte Ed25519 seed.
    pub seed: Option<String>,

    #[arg(long)]
    /// Write the generated private seed to this file.
    pub out: Option<PathBuf>,

    #[arg(long)]
    /// Write the derived public session key to this file.
    pub pubout: Option<PathBuf>,
}

#[cfg(test)]
mod tests;
