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

/// Login Portal branding, for the installation or one app.
#[derive(Debug, Subcommand)]
pub enum ThemeCommand {
    /// Show a theme, including its current revision.
    Show(ThemeTargetArgs),
    /// Replace a theme with exactly the given settings.
    Set(ThemeSetArgs),
    /// Remove a theme, so the app falls back to the installation's.
    Remove(ThemeTargetArgs),
}

/// The installation theme, or with `--client` that app's.
#[derive(Debug, Args)]
pub struct ThemeTargetArgs {
    /// OAuth client ID of the app; omitted for the installation theme.
    #[arg(long)]
    pub client: Option<String>,
    /// Theme revision for `remove`; omitted reads the current revision once.
    #[arg(long)]
    pub expected_revision: Option<u64>,
}

/// A complete theme. Anything not given is left unset.
#[derive(Debug, Args)]
pub struct ThemeSetArgs {
    /// OAuth client ID of the app; omitted for the installation theme.
    #[arg(long)]
    pub client: Option<String>,
    /// Name shown in the Portal header and page titles instead of "Trellis".
    #[arg(long)]
    pub name: Option<String>,
    /// PNG, JPEG, WebP, or SVG logo (at most 256 KiB).
    #[arg(long)]
    pub logo: Option<PathBuf>,
    /// Stylesheet loaded after the Portal's own (installation theme only).
    #[arg(long)]
    pub stylesheet: Option<PathBuf>,
    /// Light colour as NAME=#hex; NAME is one of page, surface, ink, muted,
    /// line, line-strong, primary, primary-hover, on-primary, soft.
    #[arg(long = "color", value_name = "NAME=#HEX")]
    pub colors: Vec<String>,
    /// Dark colour as NAME=#hex, with the same names as `--color`.
    #[arg(long = "dark-color", value_name = "NAME=#HEX")]
    pub dark_colors: Vec<String>,
    /// Copy as NAME=TEXT; NAME is sign_in_title, sign_in_subtitle, or footer.
    #[arg(long = "copy", value_name = "NAME=TEXT")]
    pub copy: Vec<String>,
    /// Theme revision; omitted reads the current revision once.
    #[arg(long)]
    pub expected_revision: Option<u64>,
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
    /// Review the APIs a source package's participant implements.
    Review(ApisSourceArgs),
    /// Accept the APIs a source package's participant implements, after a
    /// compatible review. An incompatible change needs `force-replace`.
    Accept(ApisSourceArgs),
    /// Force an incompatible review using AuthApiForceReplaceRequest JSON.
    /// Requires apis.forceReplace plus both explicit acknowledgement fields.
    ForceReplace { input: PathBuf },
}

/// A participant in a native source package.
#[derive(Debug, Args)]
pub struct ApisSourceArgs {
    /// Directory containing the package's `trellis.toml`.
    #[arg(long)]
    pub source: PathBuf,
    /// Participant identity; required when the package has several.
    #[arg(long)]
    pub participant: Option<String>,
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

    /// Trust only this PEM CA for the deployment's OAuth and NATS TLS, such as
    /// a Trellis-generated installation CA. Verify its fingerprint out of band.
    #[arg(long, value_name = "PATH", conflicts_with = "ca_fingerprint")]
    pub ca_certificate: Option<PathBuf>,

    /// Trust and pin the CA the deployment presents only if its SHA-256
    /// fingerprint is this value, without prompting. Without it, an untrusted
    /// CA is shown by fingerprint and pinned once you accept it.
    #[arg(long, value_name = "SHA256")]
    pub ca_fingerprint: Option<String>,
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
