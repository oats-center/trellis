use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Manage service deployments.
#[derive(Debug, Args)]
#[command(
    override_usage = "trellis svc list [OPTIONS]\n       trellis svc import <FILE> [OPTIONS]\n       trellis svc <ID> <COMMAND>"
)]
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
    /// Add a service exported from another installation, keeping its
    /// deployment ID, instances, settings, secrets and data. An import never
    /// replaces a deployment that already exists here.
    Import(SvcImportArgs),
    #[command(flatten)]
    Resource(SvcResourceAction),
}

/// Import a service bundle written by `trellis svc <ID> export`.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcImportArgs {
    /// The bundle file.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Read the bundle's passphrase from this file, or `-` for standard
    /// input. Without it, a terminal asks for it, and otherwise it is read
    /// from standard input.
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
    /// Check the bundle and show what the import would do, changing nothing.
    #[arg(long)]
    pub check: bool,
    /// Import even though another deployment here already serves an API the
    /// service provides; requests then go to either.
    #[arg(long)]
    pub acknowledge_shared_apis: bool,
    /// Import webhook endpoints that check nothing (`verify none`).
    #[arg(long)]
    pub acknowledge_unauthenticated_webhooks: bool,
}

/// Export a service for another installation.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcExportArgs {
    /// Where to write the bundle. Without it, `<ID>.trellis-service` in the
    /// current folder.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Read the passphrase that encrypts the bundle from this file, or `-`
    /// for standard input. Without it, a terminal asks for it twice, and
    /// otherwise it is read from standard input.
    #[arg(long, value_name = "PATH")]
    pub passphrase_file: Option<PathBuf>,
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
    Disable(DisableArgs),
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
    /// Show the service's webhook endpoints and the requests they received,
    /// or change an endpoint: enable or disable it, rotate its URL, or set a
    /// runtime setting.
    Webhooks(WebhooksArgs),
    /// Write the service, with its build, instances, settings, secrets and
    /// data, to a bundle encrypted with a passphrase, for `trellis svc
    /// import` in another installation. Nothing changes here: stop or remove
    /// the service yourself once it runs in the other installation.
    Export(SvcExportArgs),
}

/// A service's webhook endpoints; with an endpoint, that one.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct WebhooksArgs {
    /// The endpoint's name in the service's IDL.
    #[arg(value_name = "ENDPOINT")]
    pub endpoint: Option<String>,
    #[command(subcommand)]
    pub command: Option<WebhooksCommand>,
}

/// One webhook endpoint operation; without one, show the endpoint.
#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
pub enum WebhooksCommand {
    /// List the endpoint's request records, newest first, without bodies.
    Attempts {
        /// Only records with this outcome, such as `rejected` or `pending`.
        #[arg(long, value_name = "OUTCOME")]
        outcome: Option<String>,
        /// Only records received within this long, such as `1h` or `7d`.
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
    },
    /// Show one request record with its headers.
    Attempt {
        #[arg(value_name = "ATTEMPT_ID")]
        attempt: String,
        /// Write the stored request body to standard output.
        #[arg(long)]
        body: bool,
    },
    /// Verify a stored request again with the current verifiers and secrets.
    Reverify {
        #[arg(value_name = "ATTEMPT_ID")]
        attempt: String,
    },
    /// Accept requests again.
    Enable,
    /// Answer 503 and store nothing.
    Disable,
    /// Give the endpoint a new URL. The old one keeps working until `retire-url`.
    RotateUrl,
    /// Stop accepting the endpoint's earlier URLs.
    RetireUrl,
    /// Change a runtime setting, such as `max-body 512KiB` or `dedupe-window 24h`;
    /// `default` returns it to the IDL or Trellis default.
    Set {
        #[arg(value_name = "SETTING")]
        setting: String,
        #[arg(value_name = "VALUE")]
        value: String,
    },
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
    /// Set one or more settings and secrets as one revision, such as
    /// `logLevel=debug port=9090 --secret apiKey=@key.txt`. Any
    /// lowerCamelCase name is kept, whether or not the installed build
    /// declares it, so values for the next build can be set ahead of it.
    Set {
        /// `FIELD=VALUE`. A text setting takes the text as written; any other
        /// declared one takes JSON, such as `9090`, `true`, or `["a","b"]`.
        /// A field the build doesn't declare takes JSON when the value parses
        /// as JSON and text otherwise; quote it as JSON (`'"9090"'`) to keep
        /// digits as text. A secret is never given here; use `--secret`.
        #[arg(
            value_name = "FIELD=VALUE",
            required_unless_present_any = ["null", "secret", "values_file", "env_file"]
        )]
        values: Vec<String>,
        /// Store null for this setting, which its type must allow. Repeat it
        /// for several. A text setting would otherwise take `null` as text.
        #[arg(long, value_name = "FIELD")]
        null: Vec<String>,
        /// `FIELD=@PATH`: set a secret from a file, or `FIELD=@-` from
        /// standard input, under the same rule as `set-secret`. Repeat it for
        /// several. The value itself is never taken on the command line.
        #[arg(long, value_name = "FIELD=@PATH")]
        secret: Vec<String>,
        /// A JSON object of settings by field name, each value as JSON.
        #[arg(long, value_name = "PATH")]
        values_file: Option<PathBuf>,
        /// `NAME=VALUE` lines, read as `FIELD=VALUE` arguments are. `#`
        /// starts a comment, `export ` is ignored, and a value may be quoted.
        #[arg(long, value_name = "PATH")]
        env_file: Option<PathBuf>,
        /// Fields of `--values-file` or `--env-file` to store as secrets.
        /// Fields the installed build declares secret always are.
        #[arg(long, value_name = "FIELD,...", value_delimiter = ',')]
        secret_keys: Vec<String>,
    },
    /// Set secrets from files or standard input as one revision, so a value
    /// never appears in the shell history or process list. Given one
    /// `FIELD` and no file on a terminal, it asks for the value without
    /// showing it, twice.
    SetSecret {
        /// `FIELD`, read with `--from-file`, from piped standard input, or
        /// asked for; or one or more `FIELD=@PATH`, each read from its file
        /// (`@-` for standard input).
        #[arg(value_name = "FIELD", required_unless_present = "env_file")]
        fields: Vec<String>,
        /// Read the value of a single `FIELD` from this file, or `-` for
        /// standard input. A text secret takes the contents without the
        /// trailing newline; any other takes JSON. A field the build doesn't
        /// declare is stored as a secret under the same rule as `set`.
        /// Without it, a value piped to standard input is read, and a
        /// terminal is asked.
        #[arg(long, value_name = "PATH")]
        from_file: Option<PathBuf>,
        /// Store null for a single `FIELD` instead of reading a value. Only a
        /// secret whose type allows null takes it, such as one an instance
        /// should go without.
        #[arg(long, conflicts_with = "from_file")]
        null: bool,
        /// `NAME=VALUE` lines, each stored as a secret, read as `set
        /// --env-file` reads them.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["from_file", "null"])]
        env_file: Option<PathBuf>,
    },
    /// Print a stored secret's value to standard output. Needs
    /// `config.manageSecrets`, and Trellis records who asked for which
    /// secret, never the value. A text secret prints as written; any other
    /// prints as JSON.
    Reveal {
        #[arg(value_name = "FIELD")]
        field: String,
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
    /// Confirm webhook endpoints that check nothing (`verify none`): anyone
    /// who learns such an endpoint's URL can send it requests.
    #[arg(long)]
    pub acknowledge_unauthenticated_webhooks: bool,
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
    /// Confirm webhook endpoints that check nothing (`verify none`): anyone
    /// who learns such an endpoint's URL can send it requests.
    #[arg(long)]
    pub acknowledge_unauthenticated_webhooks: bool,
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
    /// Confirm webhook endpoints that check nothing (`verify none`): anyone
    /// who learns such an endpoint's URL can send it requests.
    #[arg(long)]
    pub acknowledge_unauthenticated_webhooks: bool,
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
    /// Confirm webhook endpoints that check nothing (`verify none`): anyone
    /// who learns such an endpoint's URL can send it requests.
    #[arg(long)]
    pub acknowledge_unauthenticated_webhooks: bool,
}

/// Remove a deployment; enforcement is performed by the native Auth runtime.
/// The confirmation names the installation it acts on.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct RemoveArgs {
    /// Remove it without being asked.
    #[arg(short = 'f', long)]
    pub force: bool,
}

/// Disable a service deployment. One imported into this installation within
/// the last day is disabled only once confirmed, naming the installation,
/// since the installation it moved from has a deployment with the same ID.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct DisableArgs {
    /// Disable it without being asked.
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
