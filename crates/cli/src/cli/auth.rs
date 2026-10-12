use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
/// Authorize this native client through the selected server's device flow.
pub struct LoginArgs {
    #[arg(value_name = "TRELLIS_URL")]
    /// Base URL for the Trellis deployment.
    pub trellis_url: String,

    /// Write the login URL to this path once it is available.
    #[arg(long = "login-url-file", value_name = "PATH")]
    pub login_url_file: Option<PathBuf>,
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
}

#[derive(Debug, Args)]
/// Manage Trellis users.
pub struct UsersCommand {
    #[command(subcommand)]
    pub command: UsersSubcommand,
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
    pub username: String,
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
}
