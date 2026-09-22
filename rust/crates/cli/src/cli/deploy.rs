use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
/// Review mode enforced for newly activated devices in a deployment.
pub enum DeviceReviewMode {
    None,
    Required,
}

impl DeviceReviewMode {
    pub fn as_wire_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
/// Allowed device instance state filters.
pub enum DeviceInstanceState {
    Registered,
    Activated,
    Revoked,
    Disabled,
}

impl DeviceInstanceState {
    pub fn as_wire_value(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Activated => "activated",
            Self::Revoked => "revoked",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
/// Allowed device activation state filters.
pub enum DeviceActivationState {
    Activated,
    Revoked,
}

impl DeviceActivationState {
    pub fn as_wire_value(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
/// Allowed device review state filters.
pub enum DeviceReviewState {
    Pending,
    Approved,
    Rejected,
}

impl DeviceReviewState {
    pub fn as_wire_value(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Args)]
/// Manage service deployments.
#[command(
    override_usage = "trellis svc list [OPTIONS]\n       trellis svc <ID> <COMMAND>",
    after_help = "In the target-first form, <ID> and <COMMAND> are required."
)]
pub struct SvcCommand {
    /// Service deployment ID for target-first actions.
    #[arg(value_name = "ID", hide = true)]
    pub id: Option<String>,

    #[command(subcommand)]
    pub command: SvcSubcommand,
}

#[derive(Debug, Subcommand)]
/// Service deployment operations.
pub enum SvcSubcommand {
    /// List service deployments.
    List(SvcListArgs),
    #[command(flatten)]
    Resource(SvcResourceAction),
}

#[derive(Debug, Args)]
/// List service deployments.
pub struct SvcListArgs {
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Debug, Args)]
/// Manage device deployments.
#[command(
    override_usage = "trellis dev list [OPTIONS]\n       trellis dev <ID> <COMMAND>",
    after_help = "In the target-first form, <ID> and <COMMAND> are required."
)]
pub struct DevCommand {
    /// Device deployment ID for target-first actions.
    #[arg(value_name = "ID", hide = true)]
    pub id: Option<String>,

    #[command(subcommand)]
    pub command: DevSubcommand,
}

#[derive(Debug, Subcommand)]
/// Device deployment operations.
pub enum DevSubcommand {
    /// List device deployments.
    List(DevListArgs),
    #[command(flatten)]
    Resource(DevResourceAction),
}

#[derive(Debug, Args)]
/// List device deployments.
pub struct DevListArgs {
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Parsed target-first service deployment command.
pub struct SvcResourceCommand {
    pub id: String,
    pub action: SvcResourceAction,
}

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
/// Actions available for one service deployment.
pub enum SvcResourceAction {
    /// Show one service deployment.
    #[command(override_usage = "trellis svc <ID> show")]
    Show,
    #[command(override_usage = "trellis svc <ID> create [OPTIONS]")]
    Create(SvcCreateArgs),
    #[command(override_usage = "trellis svc <ID> apply --source <PROJECT_ROOT> [OPTIONS]")]
    Apply(ApplyArgs),
    /// Disable one service deployment.
    #[command(override_usage = "trellis svc <ID> disable")]
    Disable,
    /// Enable one service deployment.
    #[command(override_usage = "trellis svc <ID> enable")]
    Enable,
    #[command(override_usage = "trellis svc <ID> remove [OPTIONS]")]
    Remove(RemoveArgs),
    #[command(override_usage = "trellis svc <ID> instances [OPTIONS]")]
    Instances(SvcInstancesArgs),
    #[command(override_usage = "trellis svc <ID> provision [OPTIONS]")]
    Provision(SvcProvisionArgs),
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Parsed target-first device deployment command.
pub struct DevResourceCommand {
    pub id: String,
    pub action: DevResourceAction,
}

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
/// Actions available for one device deployment.
pub enum DevResourceAction {
    /// Show one device deployment.
    #[command(override_usage = "trellis dev <ID> show")]
    Show,
    #[command(override_usage = "trellis dev <ID> create [OPTIONS]")]
    Create(DevCreateArgs),
    #[command(override_usage = "trellis dev <ID> apply --source <PROJECT_ROOT> [OPTIONS]")]
    Apply(ApplyArgs),
    /// Disable one device deployment.
    #[command(override_usage = "trellis dev <ID> disable")]
    Disable,
    /// Enable one device deployment.
    #[command(override_usage = "trellis dev <ID> enable")]
    Enable,
    #[command(override_usage = "trellis dev <ID> remove [OPTIONS]")]
    Remove(RemoveArgs),
    #[command(override_usage = "trellis dev <ID> instances [OPTIONS]")]
    Instances(DevInstancesArgs),
    #[command(override_usage = "trellis dev <ID> provision [OPTIONS]")]
    Provision(DevProvisionArgs),
    #[command(override_usage = "trellis dev <ID> activations <COMMAND>")]
    #[command(subcommand)]
    Activations(DevActivationsCommand),
    #[command(override_usage = "trellis dev <ID> reviews <COMMAND>")]
    #[command(subcommand)]
    Reviews(DevReviewsCommand),
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Create one service deployment.
pub struct SvcCreateArgs {}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Create one device deployment.
pub struct DevCreateArgs {
    #[arg(long = "review-mode", default_value = "none")]
    pub review_mode: DeviceReviewMode,
    /// Require an activating user to establish delegation independently of review.
    #[arg(long)]
    pub requires_device_delegation: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Compile and apply one participant definition from a project.
pub struct ApplyArgs {
    /// Trellis source-package root containing trellis.toml and trellis.lock.
    #[arg(long)]
    pub source: PathBuf,

    /// Participant ID when the project declares more than one matching candidate.
    #[arg(long)]
    pub participant: Option<String>,

    /// Approve one qualified capability and its current consent digest.
    #[arg(long = "approve-capability")]
    pub approve_capability: Vec<String>,

    /// Approve one participant resource at its authored commitment.
    #[arg(long = "approve-resource")]
    pub approve_resource: Vec<String>,

    /// Approve the participant's named companion.
    #[arg(long)]
    pub approve_companion: bool,

    /// Confirm the exact server-presented approval decision digest.
    #[arg(long = "confirm-digest")]
    pub confirm_digest: Option<String>,

    /// Confirm the displayed request non-interactively.
    #[arg(long)]
    pub yes: bool,

    /// Expected current GrantBinding revision; omitted reads it once.
    #[arg(long)]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Remove one service or device deployment.
pub struct RemoveArgs {
    #[arg(short = 'f', long)]
    pub force: bool,

    #[arg(long)]
    pub cascade: bool,

    #[arg(long, requires = "cascade")]
    pub purge: bool,

    #[arg(long = "purge-unused-contracts", requires = "cascade")]
    pub purge_unused_contracts: bool,
}

impl RemoveArgs {
    /// Returns whether unused deployment contract records should be purged.
    pub fn should_purge_unused_contracts(&self) -> bool {
        self.purge || self.purge_unused_contracts
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// List service instances.
pub struct SvcInstancesArgs {
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// List device instances.
pub struct DevInstancesArgs {
    #[arg(long)]
    pub state: Option<DeviceInstanceState>,

    #[arg(long = "show-metadata")]
    pub show_metadata: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Provision one service instance.
pub struct SvcProvisionArgs {
    #[arg(long = "instance-seed")]
    pub instance_seed: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Provision one device instance.
pub struct DevProvisionArgs {
    #[arg(long)]
    pub name: Option<String>,

    #[arg(long = "serial-number")]
    pub serial_number: Option<String>,

    #[arg(long = "model-number")]
    pub model_number: Option<String>,

    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
/// Device activation operations for one device deployment.
pub enum DevActivationsCommand {
    #[command(override_usage = "trellis dev <ID> activations list [OPTIONS]")]
    List(DevActivationsListArgs),
    #[command(override_usage = "trellis dev <ID> activations revoke <INSTANCE_ID>")]
    Revoke(DevActivationRevokeArgs),
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// List device activations.
pub struct DevActivationsListArgs {
    #[arg(long = "instance")]
    pub instance: Option<String>,

    #[arg(long)]
    pub state: Option<DeviceActivationState>,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Revoke one device activation.
pub struct DevActivationRevokeArgs {
    #[arg(value_name = "INSTANCE_ID")]
    pub instance_id: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
/// Device activation review operations for one device deployment.
pub enum DevReviewsCommand {
    #[command(override_usage = "trellis dev <ID> reviews list [OPTIONS]")]
    List(DevReviewsListArgs),
    #[command(override_usage = "trellis dev <ID> reviews approve <REVIEW_ID> [OPTIONS]")]
    Approve(DevReviewDecisionArgs),
    #[command(override_usage = "trellis dev <ID> reviews reject <REVIEW_ID> [OPTIONS]")]
    Reject(DevReviewDecisionArgs),
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// List device activation reviews.
pub struct DevReviewsListArgs {
    #[arg(long = "instance")]
    pub instance: Option<String>,

    #[arg(long)]
    pub state: Option<DeviceReviewState>,
}

#[derive(Debug, Clone, Eq, PartialEq, Args)]
/// Decide one device activation review.
pub struct DevReviewDecisionArgs {
    #[arg(value_name = "REVIEW_ID")]
    pub review_id: String,

    #[arg(long)]
    pub reason: Option<String>,
}
