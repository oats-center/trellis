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
    /// Register an app, asking for each setting the flags leave out.
    /// With every required flag given, or with `--no-input`, nothing is asked.
    Add {
        /// The app's client ID, such as `acme.orders-web`; it cannot change later.
        client_id: Option<String>,
        #[command(flatten)]
        settings: ClientSettingsArgs,
    },
    /// Change a registration, asking each question with its current value
    /// filled in; without a terminal, settings you leave out keep their value.
    Edit {
        client_id: String,
        #[command(flatten)]
        settings: ClientSettingsArgs,
    },
    /// Create or replace a registration from an AuthOAuthClientPutRequest JSON
    /// file, or `-` for standard input, for scripts. `add` asks instead.
    Put {
        input: PathBuf,
        /// Also attach the settings an app declares, compiled from this source
        /// project, so administrators can set them.
        #[arg(long, value_name = "PATH")]
        source: Option<PathBuf>,
        /// The `app` or `agent` in `--source` that declares the settings,
        /// such as `acme-orders.Panel`; needed when the project has several.
        /// Its IDL kind is the registration's kind.
        #[arg(long, requires = "source", value_name = "APP")]
        app: Option<String>,
    },
    /// Show or change the settings an app declares.
    Config {
        client_id: String,
        #[command(flatten)]
        args: crate::cli::ConfigArgs,
    },
    /// Choose how users may create their own account while signing in to
    /// this app; options you leave out keep their current value.
    SignUp(ClientSignUpArgs),
    /// Disable a client at the explicit expected revision. Editing it keeps
    /// it disabled.
    Disable(ClientMutationArgs),
    /// Enable a disabled client at the explicit expected revision.
    Enable(ClientMutationArgs),
    /// Delete a client at the explicit expected revision.
    Delete(ClientMutationArgs),
}

/// Whether an app runs in a web page or on a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ClientKind {
    /// A web app; users return to it at a redirect URI after signing in.
    Browser,
    /// A desktop, mobile, or command-line app, which may sign in with a code
    /// shown on another device instead of a redirect URI.
    Native,
}

/// What an OIDC provider is asked to do on the first sign-in after a sign-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SignOutPrompt {
    /// Send `prompt=login`: the provider asks for credentials again.
    Login,
    /// Send `prompt=select_account`: an account chooser, which does not prove
    /// the person signed in again.
    SelectAccount,
    /// Send `prompt=consent`: the provider asks the person to approve this
    /// Trellis again.
    Consent,
    /// Send no prompt: the provider may sign the person straight back in.
    None,
}

/// The settings of an app registration, each also asked for when left out.
#[derive(Debug, Args)]
#[command(
    group(clap::ArgGroup::new("development_state").args(["development", "no_development"])),
    group(clap::ArgGroup::new("sign_up_password_state").args(["sign_up_password", "no_sign_up_password"])),
    group(clap::ArgGroup::new("sign_up_providers_state").args(["sign_up_providers", "no_sign_up_providers"]))
)]
pub struct ClientSettingsArgs {
    /// Browser or native; taken from the IDL when `--source` names the app.
    #[arg(long, value_enum)]
    pub kind: Option<ClientKind>,
    /// Name users see when the app asks them to sign in.
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
    /// A development app may return users to plain `http://localhost`.
    /// Never mark an app users rely on as one.
    #[arg(long)]
    pub development: bool,
    #[arg(long)]
    pub no_development: bool,
    /// Where users return after signing in; repeat for each. HTTPS, or plain
    /// HTTP on localhost for a development or native app. Replaces the list.
    #[arg(long = "redirect-uri", value_name = "URI")]
    pub redirect_uris: Vec<String>,
    /// A capability the app gets without asking users, such as
    /// `acme.orders.v1:orders.read`; repeat for each. Replaces the list.
    #[arg(long = "allow-without-asking", value_name = "CAPABILITY_ID")]
    pub implied_consent: Vec<String>,
    /// Let users create a username-and-password account while signing in.
    /// The installation must also allow it (`trellis sign-in set --local-sign-up`).
    #[arg(long)]
    pub sign_up_password: bool,
    #[arg(long)]
    pub no_sign_up_password: bool,
    /// Let a first sign-in with an identity provider create the account.
    #[arg(long)]
    pub sign_up_providers: bool,
    #[arg(long)]
    pub no_sign_up_providers: bool,
    /// Also attach the settings an app declares, compiled from this source
    /// project, so administrators can set them.
    #[arg(long, value_name = "PATH")]
    pub source: Option<PathBuf>,
    /// The `app` or `agent` in `--source` that declares the settings,
    /// such as `acme-orders.Panel`; needed when the project has several.
    /// Its IDL kind is the registration's kind.
    #[arg(long, requires = "source", value_name = "APP")]
    pub app: Option<String>,
}

/// How users may create their own account while signing in to one app.
#[derive(Debug, Args)]
#[command(
    group(clap::ArgGroup::new("password_state").args(["password", "no_password"])),
    group(clap::ArgGroup::new("providers_state").args(["providers", "no_providers"]))
)]
pub struct ClientSignUpArgs {
    pub client_id: String,
    /// Allow a username and password chosen on the Portal. The installation
    /// must also allow local sign-up (`trellis sign-in set --local-sign-up`).
    #[arg(long)]
    pub password: bool,
    #[arg(long)]
    pub no_password: bool,
    /// Let a first sign-in with an identity provider create the account.
    #[arg(long)]
    pub providers: bool,
    #[arg(long)]
    pub no_providers: bool,
}

/// How people sign in to this Trellis. Changes apply at once.
#[derive(Debug, Subcommand)]
pub enum SignInCommand {
    /// Show which sign-in methods are on and the minimum password length.
    Show,
    /// Change how users sign in; options you leave out keep their value.
    /// OIDC sign-in is on while any provider is (`trellis sign-in providers`).
    Set(SignInSetArgs),
    /// Add and change the OIDC providers people sign in with.
    #[command(subcommand)]
    Providers(ProvidersCommand),
}

/// OIDC (OpenID Connect) providers.
#[derive(Debug, Subcommand)]
pub enum ProvidersCommand {
    /// List every provider with its ID, which `trellis users find` takes.
    List,
    /// Show one provider and the callback URL to register with it.
    Show { provider_id: String },
    /// Add a provider and print its ID and the callback URL to register with it.
    Add(ProviderAddArgs),
    /// Change a provider; options you leave out keep their value.
    Edit(ProviderEditArgs),
    /// Turn a provider back on.
    Enable { provider_id: String },
    /// Turn a provider off, ending its sign-ins. It stays listed until removed.
    Disable { provider_id: String },
    /// Delete a provider, ending its sign-ins. Accounts keep their other ways in.
    Remove { provider_id: String },
}

#[derive(Debug, Args)]
pub struct ProviderAddArgs {
    /// Shown on the sign-in button.
    #[arg(long)]
    pub name: Option<String>,
    /// The provider's issuer URL, such as `https://example.us.auth0.com/`.
    #[arg(long)]
    pub issuer: Option<String>,
    /// The client ID the provider gave this Trellis.
    #[arg(long)]
    pub client_id: Option<String>,
    /// File holding the client secret, or `-` to read it from standard input.
    #[arg(long, value_name = "PATH")]
    pub client_secret_file: Option<PathBuf>,
    /// A scope to request; repeat for each. Defaults to openid, profile and email.
    #[arg(long = "scope", value_name = "SCOPE")]
    pub scopes: Vec<String>,
    /// JSON pointer to the claim listing a user's groups, for giving roles.
    /// Defaults to `/groups`.
    #[arg(long)]
    pub groups_claim: Option<String>,
    /// Trust the provider's certificate issuer with this SHA-256 fingerprint,
    /// for a provider whose certificate nothing on the server vouches for.
    /// Confirm it with the provider's operator first.
    #[arg(long, value_name = "FINGERPRINT")]
    pub certificate_fingerprint: Option<String>,
    /// What the provider is asked to do on the first sign-in after a sign-out.
    /// Use another value only for a provider that refuses `login`. Defaults
    /// to `login`.
    #[arg(long, value_enum, value_name = "PROMPT")]
    pub prompt_after_sign_out: Option<SignOutPrompt>,
    /// A parameter added to every authorization request sent to the provider,
    /// such as `organization=org_123` for Auth0 Organizations; repeat for each.
    #[arg(long = "authorization-param", value_name = "NAME=VALUE", value_parser = authorization_param)]
    pub authorization_params: Vec<(String, String)>,
}

#[derive(Debug, Args)]
pub struct ProviderEditArgs {
    pub provider_id: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub client_id: Option<String>,
    /// File holding a new client secret, or `-` to read it from standard input.
    #[arg(long, value_name = "PATH")]
    pub client_secret_file: Option<PathBuf>,
    /// Replace the requested scopes; repeat for each.
    #[arg(long = "scope", value_name = "SCOPE")]
    pub scopes: Vec<String>,
    #[arg(long)]
    pub groups_claim: Option<String>,
    /// Trust the provider's certificate issuer with this SHA-256 fingerprint.
    #[arg(
        long,
        value_name = "FINGERPRINT",
        conflicts_with = "no_certificate_fingerprint"
    )]
    pub certificate_fingerprint: Option<String>,
    /// Stop trusting a pinned certificate issuer; use the system trust store alone.
    #[arg(long)]
    pub no_certificate_fingerprint: bool,
    /// What the provider is asked to do on the first sign-in after a sign-out.
    #[arg(long, value_enum, value_name = "PROMPT")]
    pub prompt_after_sign_out: Option<SignOutPrompt>,
    /// Replace the parameters added to every authorization request; repeat
    /// for each.
    #[arg(
        long = "authorization-param",
        value_name = "NAME=VALUE",
        value_parser = authorization_param,
        conflicts_with = "clear_authorization_params"
    )]
    pub authorization_params: Vec<(String, String)>,
    /// Stop adding custom parameters to authorization requests.
    #[arg(long)]
    pub clear_authorization_params: bool,
}

/// Splits a `--authorization-param` at its first `=`.
fn authorization_param(argument: &str) -> Result<(String, String), String> {
    argument
        .split_once('=')
        .map(|(name, value)| (name.trim().to_owned(), value.to_owned()))
        .ok_or_else(|| format!("expected NAME=VALUE, got {argument:?}"))
}

#[derive(Debug, Args)]
#[command(
    group(clap::ArgGroup::new("local_users_state").args(["local_users", "no_local_users"])),
    group(clap::ArgGroup::new("local_sign_up_state").args(["local_sign_up", "no_local_sign_up"]))
)]
pub struct SignInSetArgs {
    /// Sign in with a username and password.
    #[arg(long)]
    pub local_users: bool,
    #[arg(long)]
    pub no_local_users: bool,
    /// Let people create their own username-and-password account, in apps
    /// that also allow it (`trellis clients sign-up`).
    #[arg(long)]
    pub local_sign_up: bool,
    #[arg(long)]
    pub no_local_sign_up: bool,
    /// Shortest password accepted for new passwords, changes and sign-up,
    /// 8 to 128 characters (Unicode code points, not bytes). Existing
    /// passwords are not checked again.
    #[arg(long, value_name = "LENGTH", value_parser = clap::value_parser!(u32).range(8..=128))]
    pub password_min_length: Option<u32>,
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
    /// Replace accepted APIs with a source package's incompatible definitions,
    /// asking for what the flags leave out; or, for scripts, send an
    /// AuthApiForceReplaceRequest JSON file (`-` for standard input).
    ForceReplace(ApisForceReplaceArgs),
}

/// What an incompatible replacement needs.
#[derive(Debug, Args)]
pub struct ApisForceReplaceArgs {
    /// AuthApiForceReplaceRequest JSON file, or `-` for standard input.
    #[arg(conflicts_with_all = ["source", "participant", "acknowledge_breakage", "acknowledge_consent_impact"])]
    pub input: Option<PathBuf>,
    /// Directory containing the package's `trellis.toml`.
    #[arg(long)]
    pub source: Option<PathBuf>,
    /// Participant identity; asked for when the package has several.
    #[arg(long)]
    pub participant: Option<String>,
    /// Accept that callers built against the accepted definition may break.
    #[arg(long)]
    pub acknowledge_breakage: bool,
    /// Accept that what users agreed to may now mean something different.
    #[arg(long)]
    pub acknowledge_consent_impact: bool,
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
    /// Start signing with a new issuer key, retiring the current one.
    Rotate(IssuersRotateArgs),
    /// Revoke one issuer key.
    Revoke(IssuersRevokeArgs),
}

#[derive(Debug, Args)]
/// Start signing with a new issuer key, retiring the current one.
///
/// Sessions keep working: authority the retired key signed stays valid until
/// it expires, and sessions renew onto the new key. Revoke the retired key to
/// end its authority at once.
pub struct IssuersRotateArgs {
    /// ID of the current signing key. Auth refuses the rotation if another
    /// key has become current since.
    #[arg(allow_hyphen_values = true)]
    pub current_key_id: String,
}

#[derive(Debug, Args)]
/// Revoke one issuer key.
pub struct IssuersRevokeArgs {
    /// Issuer key ID.
    #[arg(allow_hyphen_values = true)]
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
    /// Show one user.
    Show(UserRefArgs),
    /// Find the user an OIDC sign-in belongs to, by provider ID and exact subject.
    Find(UserIdentityArgs),
    /// Create one Trellis user, optionally with an OIDC sign-in linked.
    Create(UserCreateArgs),
    /// Edit one Trellis user.
    Edit(UserEditArgs),
    /// Delete one Trellis user, signing them out everywhere.
    Delete(UserRefArgs),
    /// Link an OIDC sign-in to a user, so that sign-in opens their account.
    Link(UserIdentityChangeArgs),
    /// Remove an OIDC sign-in from a user and end the sign-ins made with it.
    Unlink(UserIdentityChangeArgs),
}

#[derive(Debug, Args)]
/// Manage roles: named sets of capabilities given to users.
pub struct RolesCommand {
    #[command(subcommand)]
    pub command: RolesSubcommand,
}

#[derive(Debug, Subcommand)]
/// Role administration operations.
pub enum RolesSubcommand {
    /// List roles.
    List,
    /// Show one role and the users who have it.
    Show(RoleRefArgs),
    /// Create one role.
    Create(RoleCreateArgs),
    /// Edit one role; options you leave out keep their current value.
    Edit(RoleEditArgs),
    /// Delete one role and remove it from everyone who has it.
    Delete(RoleRefArgs),
    /// Give one role to one user, or renew one that expired.
    Assign(RoleAssignArgs),
    /// Take one role away from one user.
    Revoke(RoleMemberArgs),
}

#[derive(Debug, Args)]
/// Reference one role and one user.
pub struct RoleMemberArgs {
    /// The role, by its name or ID.
    #[arg(value_name = "ROLE")]
    pub role: String,

    /// The user, by username, email or ID.
    #[arg(long, value_name = "USER")]
    pub user: String,
}

#[derive(Debug, Args)]
/// Give one role to one user.
pub struct RoleAssignArgs {
    #[command(flatten)]
    pub member: RoleMemberArgs,

    /// Renew the user's saved assignment of this role, at the revision Auth
    /// names when it refuses a new one because the saved one expired. The
    /// renewed assignment doesn't expire.
    #[arg(long, value_name = "REVISION")]
    pub revision: Option<u64>,
}

#[derive(Debug, Args)]
/// Reference one role.
pub struct RoleRefArgs {
    /// The role, by its name or ID.
    #[arg(value_name = "ROLE")]
    pub role: String,
}

#[derive(Debug, Args)]
/// Create one role.
#[command(group(
    clap::ArgGroup::new("new_users_state")
        .args(["new_users", "no_new_users"])
        .multiple(false)
))]
pub struct RoleCreateArgs {
    #[arg(long)]
    pub title: Option<String>,

    #[arg(long)]
    pub description: Option<String>,

    /// Capability the role gives, such as `trellis.auth.v1:users.manage`.
    /// Repeat for each capability.
    #[arg(long = "capability", value_name = "CAPABILITY_ID")]
    pub capabilities: Vec<String>,

    /// Give this role to every user created from now on.
    #[arg(long)]
    pub new_users: bool,

    /// Don't give this role to new users (the default).
    #[arg(long)]
    pub no_new_users: bool,
}

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("new_users_state")
        .args(["new_users", "no_new_users"])
        .multiple(false)
))]
/// Edit one role.
pub struct RoleEditArgs {
    /// The role, by its name or ID.
    #[arg(value_name = "ROLE")]
    pub role: String,

    #[arg(long)]
    pub title: Option<String>,

    #[arg(long)]
    pub description: Option<String>,

    /// Replace the role's capabilities. Repeat for each capability.
    #[arg(long = "capability", value_name = "CAPABILITY_ID")]
    pub capabilities: Option<Vec<String>>,

    /// Give this role to every user created from now on.
    #[arg(long)]
    pub new_users: bool,

    /// Stop giving this role to new users; users who have it keep it.
    #[arg(long)]
    pub no_new_users: bool,
}

#[derive(Debug, Args)]
/// Reference one user.
pub struct UserRefArgs {
    /// The user, by username, email or ID.
    #[arg(value_name = "USER")]
    pub user: String,
}

#[derive(Debug, Args)]
/// An OIDC sign-in: a provider and the provider's subject for the user.
pub struct UserIdentityArgs {
    /// The sign-in provider's Trellis ID.
    #[arg(long, value_name = "PROVIDER_ID")]
    pub provider: Option<String>,
    /// The provider's `sub` for the user, matched exactly. It is opaque and
    /// may start with `-`.
    #[arg(long, value_name = "SUBJECT", allow_hyphen_values = true)]
    pub subject: Option<String>,
}

#[derive(Debug, Args)]
/// Link or unlink one OIDC sign-in for one user.
pub struct UserIdentityChangeArgs {
    /// The user, by username, email or ID.
    #[arg(value_name = "USER")]
    pub user: String,

    #[command(flatten)]
    pub identity: UserIdentityArgs,
}

#[derive(Debug, Args)]
/// Create one Trellis user.
pub struct UserCreateArgs {
    /// Name shown for the user; defaults to the username.
    #[arg(long)]
    pub name: Option<String>,

    #[arg(long)]
    pub username: Option<String>,

    /// The user's email address, by which commands can also name them.
    #[arg(long)]
    pub email: Option<String>,

    /// Link this OIDC sign-in to the new user. A user created with one gets no
    /// password setup link.
    #[command(flatten)]
    pub identity: UserIdentityArgs,
}

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("active_state")
        .args(["active", "inactive"])
        .multiple(false)
))]
/// Edit one Trellis user.
pub struct UserEditArgs {
    /// The user, by username, email or ID.
    #[arg(value_name = "USER")]
    pub user: String,

    #[arg(long)]
    pub active: bool,

    #[arg(long)]
    pub inactive: bool,

    #[arg(long)]
    pub name: Option<String>,
}
