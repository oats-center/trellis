use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
/// Start a detached portal login against an auth service.
pub struct LoginArgs {
    #[arg(value_name = "TRELLIS_URL")]
    /// Base URL for the Trellis deployment.
    pub trellis_url: String,
}

#[derive(Debug, Args)]
/// Manage identity-owned authority and grants.
pub struct IdentityCommand {
    #[command(subcommand)]
    pub command: IdentitySubcommand,
}

#[derive(Debug, Subcommand)]
/// Identity grant operations.
pub enum IdentitySubcommand {
    /// Inspect or replace participant-scoped user grants.
    Grants(IdentityGrantsCommand),
}

#[derive(Debug, Args)]
/// Manage participant-scoped user grants.
pub struct IdentityGrantsCommand {
    #[command(subcommand)]
    pub command: IdentityGrantsSubcommand,
}

#[derive(Debug, Subcommand)]
/// Identity grant operations.
pub enum IdentityGrantsSubcommand {
    /// Filter user grants by owner or participant.
    List(IdentityGrantsListArgs),
    /// Inspect one participant grant.
    Get(IdentityGrantsGetArgs),
    /// Replace one participant grant.
    Set(IdentityGrantsSetArgs),
    /// Revoke one participant grant.
    Revoke(IdentityGrantsRevokeArgs),
}

#[derive(Debug, Args)]
/// Install participant definitions.
pub struct ParticipantsCommand {
    #[command(subcommand)]
    pub command: ParticipantsSubcommand,
}

#[derive(Debug, Subcommand)]
/// Participant definition operations.
pub enum ParticipantsSubcommand {
    /// Compile and install one participant definition.
    Install(ParticipantsInstallArgs),
}

#[derive(Debug, Args)]
/// Compile and install one participant definition.
pub struct ParticipantsInstallArgs {
    #[arg(long)]
    /// Trellis source-package root containing trellis.toml and trellis.lock.
    pub source: PathBuf,
    #[arg(long)]
    /// Participant ID when the project declares more than one candidate.
    pub participant: Option<String>,
    #[arg(long)]
    /// Expected installed revision; omitted reads it once.
    pub expected_revision: Option<u64>,
    #[arg(long)]
    /// Mark this exact package digest as operator-trusted platform evidence.
    pub platform_trust: bool,
}

#[derive(Debug, Args)]
/// Manage authorization signing issuers.
pub struct IssuersCommand {
    #[command(subcommand)]
    pub command: IssuersSubcommand,
}

#[derive(Debug, Subcommand)]
/// Issuer operations.
pub enum IssuersSubcommand {
    /// Revoke one issuer key.
    Revoke(IssuersRevokeArgs),
}

#[derive(Debug, Args)]
/// Revoke one issuer key.
pub struct IssuersRevokeArgs {
    /// Issuer key ID.
    pub key_id: String,
    #[arg(long)]
    /// Operator-visible revocation reason.
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
/// Filter user grants by owner or participant.
pub struct IdentityGrantsListArgs {
    #[arg(long)]
    /// Restrict results to grants stored for one Trellis user ID.
    pub user: Option<String>,

    #[arg(long)]
    /// Restrict results to one installed participant ID.
    pub participant: Option<String>,
}

#[derive(Debug, Args)]
/// Inspect one participant grant.
pub struct IdentityGrantsGetArgs {
    /// Installed participant ID.
    pub participant_id: String,
    #[arg(long)]
    /// Grant owner; defaults to the authenticated user.
    pub user: Option<String>,
}

#[derive(Debug, Args)]
/// Replace one participant grant from exact JSON input.
pub struct IdentityGrantsSetArgs {
    /// Installed participant ID.
    pub participant_id: String,
    #[arg(long)]
    /// Trellis user that owns the grant.
    pub user: String,
    #[arg(long)]
    /// JSON file containing installedRevision, grants, platformPrivileges, and expiresAt.
    pub input: PathBuf,
    #[arg(long)]
    /// Expected current grant revision; omitted reads it once.
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Args)]
/// Revoke one participant grant.
pub struct IdentityGrantsRevokeArgs {
    /// Installed participant ID.
    pub participant_id: String,

    #[arg(long)]
    /// Grant owner; defaults to the authenticated user.
    pub user: Option<String>,
    #[arg(long)]
    /// Expected current grant revision; omitted reads it once.
    pub expected_revision: Option<u64>,
    #[arg(long)]
    /// Operator-visible revocation reason.
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
/// Manage Trellis users.
pub struct UsersCommand {
    #[command(subcommand)]
    pub command: UsersSubcommand,
}

#[derive(Debug, Args)]
/// Inspect and manage login portal admin surfaces.
pub struct PortalsCommand {
    #[command(subcommand)]
    pub command: PortalsSubcommand,
}

#[derive(Debug, Subcommand)]
/// Portal registry and login portal policy operations.
pub enum PortalsSubcommand {
    /// List visible login portals.
    List,
    /// Inspect login portal admin surfaces.
    Login(PortalsLoginCommand),
}

#[derive(Debug, Args)]
/// Inspect built-in login settings and selection routes.
pub struct PortalsLoginCommand {
    #[command(subcommand)]
    pub command: PortalsLoginSubcommand,
}

#[derive(Debug, Subcommand)]
/// Built-in login portal settings and route selection operations.
pub enum PortalsLoginSubcommand {
    /// Show built-in login registration defaults.
    Default,
    /// List login route selection rules.
    Selection,
}

#[derive(Debug, Subcommand)]
/// User administration operations.
pub enum UsersSubcommand {
    /// List users.
    List,
    /// Show one user by Trellis user ID.
    Show(UserRefArgs),
    /// Create one Trellis user.
    Create(UserCreateArgs),
    /// Edit one Trellis user.
    Edit(UserEditArgs),
}

#[derive(Debug, Args)]
/// Reference one user by Trellis user ID.
pub struct UserRefArgs {
    #[arg(value_name = "USER_ID")]
    pub user_id: String,
}

#[derive(Debug, Args)]
/// Create one Trellis user.
pub struct UserCreateArgs {
    #[arg(long)]
    pub name: Option<String>,

    #[arg(long)]
    pub email: Option<String>,

    #[arg(long)]
    pub username: Option<String>,

    #[arg(long)]
    pub inactive: bool,
}

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("active_state")
        .args(["active", "inactive"])
        .multiple(false)
))]
/// Edit one Trellis user.
pub struct UserEditArgs {
    #[arg(value_name = "USER_ID")]
    pub user_id: String,

    #[arg(long)]
    pub active: bool,

    #[arg(long)]
    pub inactive: bool,

    #[arg(long)]
    pub name: Option<String>,

    #[arg(long)]
    pub email: Option<String>,
}
