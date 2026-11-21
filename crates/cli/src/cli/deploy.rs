use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Manage service deployments.
#[derive(Debug, Args)]
#[command(override_usage = "trellis svc list [OPTIONS]\n       trellis svc <ID> <COMMAND>")]
pub struct SvcCommand {
    /// Deployment ID or title; creation uses this as the title.
    #[arg(value_name = "ID", hide = true)]
    pub id: Option<String>,
    #[command(subcommand)]
    pub command: SvcSubcommand,
}

/// Service deployment operations.
#[derive(Debug, Subcommand)]
pub enum SvcSubcommand {
    List(SvcListArgs),
    #[command(flatten)]
    Resource(SvcResourceAction),
}

/// Include disabled deployments in the service listing.
#[derive(Debug, Args)]
pub struct SvcListArgs {
    #[arg(long)]
    pub disabled: bool,
}

/// Manage device deployments.
#[derive(Debug, Args)]
#[command(override_usage = "trellis dev list [OPTIONS]\n       trellis dev <ID> <COMMAND>")]
pub struct DevCommand {
    /// Deployment ID or title; creation uses this as the title.
    #[arg(value_name = "ID", hide = true)]
    pub id: Option<String>,
    #[command(subcommand)]
    pub command: DevSubcommand,
}

/// Device deployment operations.
#[derive(Debug, Subcommand)]
pub enum DevSubcommand {
    List(DevListArgs),
    #[command(flatten)]
    Resource(DevResourceAction),
}

/// Include disabled deployments in the device listing.
#[derive(Debug, Args)]
pub struct DevListArgs {
    #[arg(long)]
    pub disabled: bool,
}

/// Target-first service command.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SvcResourceCommand {
    pub id: String,
    pub action: SvcResourceAction,
}

/// Operations on a service deployment.
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum SvcResourceAction {
    Show,
    Create(SvcCreateArgs),
    Apply(ApplyArgs),
    Disable,
    Enable,
    Remove(RemoveArgs),
    Instances(SvcInstancesArgs),
    Provision(SvcProvisionArgs),
    /// List updates the service submitted for itself, newest first.
    Updates,
    /// Approve the service's pending update.
    Approve(SvcUpdateArgs),
    /// Reject the service's pending update.
    Reject(SvcUpdateArgs),
    /// Show or change the settings Trellis keeps for the service. A service
    /// added without its IDL takes settings while it waits for approval, and
    /// approving it carries them over.
    Config(ConfigArgs),
}

/// Settings Trellis keeps for a deployment, or for one instance's overrides
/// with `--instance`.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct ConfigArgs {
    /// An instance of the deployment, by ID or title, whose overrides to show or change.
    #[arg(long, global = true, value_name = "INSTANCE")]
    pub instance: Option<String>,
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

/// One settings operation; without one, `show`.
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum ConfigCommand {
    /// Show each setting, its value, and where the value comes from.
    /// Secrets show only whether they are set.
    Show,
    /// Set one or more settings, such as `logLevel=debug port=9090`. Any
    /// lowerCamelCase name is kept, whether or not the installed build
    /// declares it, so values for the next build can be set ahead of it.
    Set {
        /// `FIELD=VALUE`. A text setting takes the text as written; any other
        /// declared one takes JSON, such as `9090`, `true`, or `["a","b"]`.
        /// A field the build doesn't declare takes JSON when the value parses
        /// as JSON and text otherwise; quote it as JSON (`'"9090"'`) to keep
        /// digits as text.
        #[arg(value_name = "FIELD=VALUE", required_unless_present = "null")]
        values: Vec<String>,
        /// Store null for this setting, which its type must allow. Repeat it
        /// for several. A text setting would otherwise take `null` as text.
        #[arg(long, value_name = "FIELD")]
        null: Vec<String>,
    },
    /// Set a secret from a file or standard input, so the value never
    /// appears in the shell history or process list. On a terminal given
    /// neither, it asks for the value without showing it, twice.
    SetSecret {
        #[arg(value_name = "FIELD")]
        field: String,
        /// Read the value from this file, or `-` for standard input. A text
        /// secret takes the contents without the trailing newline; any other
        /// takes JSON. A field the build doesn't declare is stored as a
        /// secret under the same rule as `set`. Without it, a value piped to
        /// standard input is read, and a terminal is asked.
        #[arg(long, value_name = "PATH")]
        from_file: Option<PathBuf>,
        /// Store null instead of reading a value. Only a secret whose type
        /// allows null takes it, such as one an instance should go without.
        #[arg(long, conflicts_with = "from_file")]
        null: bool,
    },
    /// Remove stored values so the default, or the deployment's value for an
    /// instance, applies.
    Unset {
        #[arg(value_name = "FIELD", required = true)]
        fields: Vec<String>,
    },
    /// List revisions, newest first.
    History,
    /// Make an earlier revision's values current again, as a new revision.
    Rollback {
        #[arg(value_name = "REVISION")]
        revision: u32,
    },
    /// Delete an old revision, such as one holding a replaced password.
    DeleteRevision {
        #[arg(value_name = "REVISION")]
        revision: u32,
    },
}

/// Select a service update; omitted selects the deployment's pending one.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcUpdateArgs {
    #[arg(value_name = "UPDATE_ID")]
    pub update: Option<String>,
    /// Confirm that the service may answer an API another service already
    /// serves, splitting callers' requests between the two.
    #[arg(long)]
    pub acknowledge_shared_apis: bool,
}

/// Target-first device command.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DevResourceCommand {
    pub id: String,
    pub action: DevResourceAction,
}

/// Operations on a device deployment.
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum DevResourceAction {
    Show,
    Create(DevCreateArgs),
    Apply(ApplyArgs),
    Disable,
    Enable,
    Remove(RemoveArgs),
    Instances(DevInstancesArgs),
    Provision(DevProvisionArgs),
    /// Show or change the settings Trellis keeps for the device.
    Config(ConfigArgs),
}

/// Where a deployment's build comes from: a local source package, or a
/// release published to a registry Trellis pulls from.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct BuildArgs {
    /// Directory containing the package's `trellis.toml`.
    #[arg(long, group = "build")]
    pub source: Option<PathBuf>,
    /// A published release by its full identity, `WORKSPACE/NAME@VERSION` or
    /// `WORKSPACE/NAME@sha256:DIGEST`, such as `ghcr.io/acme/orders@1.2.0`.
    /// Trellis pulls it from its own workspace, signing in with the registry
    /// added for that workspace, checks that it declares that identity and
    /// its digests, and builds it from the exact dependencies it was
    /// published with.
    #[arg(long, value_name = "WORKSPACE/NAME@VERSION", group = "build")]
    pub package: Option<String>,
    /// Participant identity; asked for when the package has several.
    #[arg(long, requires = "build")]
    pub participant: Option<String>,
}

/// Create a service deployment from its native source package or a published
/// release of it. Without `--source` or `--package`, create a claim instead:
/// the first service to connect with the claim's identity submits its own
/// IDL, and approving it creates the deployment.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcCreateArgs {
    #[command(flatten)]
    pub build: BuildArgs,
    /// For a claim: the public key the service's owner made with
    /// `trellis keys new`, so only the owner holds the seed. Without it the
    /// CLI generates an identity and prints its seed once.
    #[arg(
        long,
        value_name = "KEY",
        conflicts_with = "build",
        allow_hyphen_values = true
    )]
    pub public_key: Option<String>,
    /// Confirm that this service may answer an API another service already
    /// serves, splitting callers' requests between the two.
    #[arg(long)]
    pub acknowledge_shared_apis: bool,
}

/// Create a device deployment from its native source package or a published
/// release of it; one of `--source` and `--package` is required.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct DevCreateArgs {
    #[command(flatten)]
    pub build: BuildArgs,
    /// Confirm that this service may answer an API another service already
    /// serves, splitting callers' requests between the two.
    #[arg(long)]
    pub acknowledge_shared_apis: bool,
}

/// Install a build on an existing deployment, from its source package or a
/// published release, so instances running that build connect at once; one
/// of `--source` and `--package` is required.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct ApplyArgs {
    #[command(flatten)]
    pub build: BuildArgs,
    /// Deployment revision; omitted reads the current revision once.
    #[arg(long)]
    pub expected_revision: Option<u64>,
    /// Confirm that this service may answer an API another service already
    /// serves, splitting callers' requests between the two.
    #[arg(long)]
    pub acknowledge_shared_apis: bool,
}

/// Remove a deployment; enforcement is performed by the native Auth runtime.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct RemoveArgs {
    #[arg(short = 'f', long)]
    pub force: bool,
}

/// List service instances, optionally including disabled principals.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcInstancesArgs {
    #[arg(long)]
    pub disabled: bool,
}

/// List device instances, optionally including disabled principals.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct DevInstancesArgs {
    #[arg(long)]
    pub disabled: bool,
}

/// Provision or select a service identity.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcProvisionArgs {
    /// Register this public key, made by the service's owner with
    /// `trellis keys new`, or select the instance that already has it. Only
    /// the owner holds the seed. Without it the CLI selects the one existing
    /// instance, or generates an identity and prints its seed once.
    #[arg(long, value_name = "KEY", allow_hyphen_values = true)]
    pub public_key: Option<String>,
    /// Create a replica instead of selecting an existing identity.
    #[arg(long)]
    pub new: bool,
}

/// Provision a device identity with an optional title.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct DevProvisionArgs {
    #[arg(long)]
    pub name: Option<String>,
}

/// Registries Trellis pulls published packages from.
#[derive(Debug, Subcommand)]
pub enum RegistriesCommand {
    /// List each registry with its prefix and the user it signs in as.
    List,
    /// Add a registry, or replace the one with the same name.
    Set(RegistrySetArgs),
    /// Remove a registry and its stored credentials.
    Remove {
        #[arg(value_name = "NAME")]
        name: String,
    },
}

/// One registry and how Trellis signs in to it.
#[derive(Debug, Args)]
pub struct RegistrySetArgs {
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Registry host and repository prefix, such as `ghcr.io/oats-center`.
    #[arg(long, value_name = "PREFIX")]
    pub prefix: String,
    /// Sign in as this user, for a private registry. Without it Trellis pulls
    /// anonymously.
    #[arg(long, value_name = "USER")]
    pub username: Option<String>,
    /// Read the password or token from this file, or `-` for standard input.
    /// Without it, a piped value is read and a terminal is asked.
    #[arg(long, value_name = "PATH", requires = "username")]
    pub secret_file: Option<PathBuf>,
}
