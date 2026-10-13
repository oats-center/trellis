use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Public OAuth client registration operations.
#[derive(Debug, Subcommand)]
pub enum ClientsCommand {
    /// List one page of registered clients.
    List {
        /// Opaque next-page cursor from the preceding result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read one registration, including its current revision.
    Get { client_id: String },
    /// Create or replace a registration using an AuthOAuthClientPutRequest JSON file.
    /// Use kind=browser, development=true and exact redirectUris for localhost apps.
    Put { input: PathBuf },
    /// Disable a client at the explicit expected revision.
    Disable(ClientMutationArgs),
    /// Delete a client at the explicit expected revision.
    Delete(ClientMutationArgs),
}

/// Revision-fenced public client mutation.
#[derive(Debug, Args)]
pub struct ClientMutationArgs {
    pub client_id: String,
    #[arg(long)]
    pub expected_revision: u64,
}

/// Accepted API catalog operations using generated request JSON.
#[derive(Debug, Subcommand)]
pub enum ApisCommand {
    /// List one page of accepted APIs.
    List {
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Inspect the accepted API and canonical IDL.
    Get { api_id: String },
    /// Review an AuthApiReviewRequest JSON file containing exact package evidence.
    Review { input: PathBuf },
    /// Accept an AuthApiAcceptRequest JSON file from a compatible review.
    Accept { input: PathBuf },
    /// Force an incompatible review using AuthApiForceReplaceRequest JSON.
    /// Requires apis.forceReplace plus both explicit acknowledgement fields.
    ForceReplace { input: PathBuf },
}

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
