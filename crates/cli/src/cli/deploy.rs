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
    /// Issue, list, change and revoke the secrets that let processes enroll
    /// themselves as new instances of this service.
    #[command(subcommand)]
    Enrollment(EnrollmentCommand),
    /// Show or change this service's own rules for retiring offline
    /// instances; unset rules follow `trellis instance-policy`.
    #[command(subcommand)]
    InstancePolicy(DeploymentPolicyCommand),
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
    /// A published release, `NAME@VERSION` or `NAME@sha256:DIGEST`. Trellis
    /// pulls it from `--registry`, checks its digests, and builds it from the
    /// exact dependencies it was published with.
    #[arg(long, value_name = "NAME@VERSION", group = "build")]
    pub package: Option<String>,
    /// The registry to pull `--package` from, by the name `trellis registries
    /// list` shows; needed when Trellis has more than one.
    #[arg(long, value_name = "NAME", requires = "package")]
    pub registry: Option<String>,
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
    /// Include retired and removed instances.
    #[arg(long)]
    pub all: bool,
}

/// Enrollment-secret operations.
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum EnrollmentCommand {
    /// Issue a secret and print it once on standard output, so it can be
    /// piped into a secret store. Trellis keeps only its hash.
    Issue(EnrollmentIssueArgs),
    /// List the service's secrets: limits, instances, last use, and whether
    /// each is revoked.
    List,
    /// Change a secret's title and limits; flags left out keep their value.
    Edit(EnrollmentEditArgs),
    /// Stop a secret from creating instances.
    Revoke(EnrollmentRevokeArgs),
}

/// A secret's title and limits.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct EnrollmentLimitArgs {
    #[arg(long)]
    pub title: Option<String>,
    /// When it stops creating instances: a date (`2027-01-31`), a time
    /// (RFC 3339), or a duration from now (`90d`, `12h`). `never` clears it.
    #[arg(long, value_name = "DATE|DURATION")]
    pub expires: Option<String>,
    /// The most instances it may have at once.
    #[arg(long, value_name = "N")]
    pub max_instances: Option<u32>,
    /// The most new instances it may create in any hour.
    #[arg(long, value_name = "N")]
    pub per_hour: Option<u32>,
    /// Its instances may only run the installed build: their own updates are
    /// refused, so a new build is installed with `apply` first.
    #[arg(long, value_name = "on|off", num_args = 0..=1, default_missing_value = "on")]
    pub current_build_only: Option<String>,
}

/// Issue a secret.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct EnrollmentIssueArgs {
    #[command(flatten)]
    pub limits: EnrollmentLimitArgs,
}

/// Change one secret.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct EnrollmentEditArgs {
    #[arg(value_name = "SECRET_ID")]
    pub secret: String,
    #[command(flatten)]
    pub limits: EnrollmentLimitArgs,
}

/// Revoke one secret.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct EnrollmentRevokeArgs {
    #[arg(value_name = "SECRET_ID")]
    pub secret: String,
    /// Also retire every instance it created, which disconnects them: the
    /// response to a leaked secret.
    #[arg(long)]
    pub retire_instances: bool,
}

/// One service's own retirement rules.
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum DeploymentPolicyCommand {
    /// Show the service's own rules and the ones that apply to it.
    Show,
    /// Change the service's own rules; `default` follows the installation.
    Set(DeploymentPolicySetArgs),
}

/// One service's own retirement rules.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct DeploymentPolicySetArgs {
    /// How long an instance may stay offline before it is retired (`24h`,
    /// at least `1h`), `off`, or `default`.
    #[arg(long, value_name = "DURATION|off|default")]
    pub retire_offline_after: Option<String>,
    /// Whether provisioned instances are retired too: `on`, `off`, or `default`.
    #[arg(long, value_name = "on|off|default")]
    pub retire_provisioned: Option<String>,
}

/// The installation's rules for enrolled and offline instances.
#[derive(Debug, Subcommand)]
pub enum InstancePolicyCommand {
    /// Show the installation's rules.
    Show,
    /// Change the installation's rules; flags left out keep their value.
    Set(InstancePolicySetArgs),
}

/// The installation's rules for enrolled and offline instances.
#[derive(Debug, Args)]
pub struct InstancePolicySetArgs {
    /// How long an instance may stay offline before it is retired (`24h`,
    /// at least `1h`), or `off`.
    #[arg(long, value_name = "DURATION|off")]
    pub retire_offline_after: Option<String>,
    /// Whether provisioned instances are retired too.
    #[arg(long, value_name = "on|off")]
    pub retire_provisioned: Option<String>,
    /// Days a retired or removed instance's record is kept.
    #[arg(long, value_name = "N")]
    pub retired_record_days: Option<u32>,
    /// How long a just-enrolled instance that has not connected yet is
    /// protected from being replaced (`10m`).
    #[arg(long, value_name = "DURATION")]
    pub first_connect_grace: Option<String>,
    /// The instance cap a new secret takes.
    #[arg(long, value_name = "N")]
    pub secret_max_instances: Option<u32>,
    /// The hourly rate a new secret takes.
    #[arg(long, value_name = "N")]
    pub secret_per_hour: Option<u32>,
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
