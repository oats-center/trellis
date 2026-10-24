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
}

/// Create a service deployment from its native source package.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct SvcCreateArgs {
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long)]
    pub participant: Option<String>,
}

/// Create a device deployment from its native source package.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct DevCreateArgs {
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long)]
    pub participant: Option<String>,
}

/// Apply an implementation package to an existing deployment.
#[derive(Debug, Clone, Eq, PartialEq, Args)]
pub struct ApplyArgs {
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long)]
    pub participant: Option<String>,
    /// Deployment revision; omitted reads the current revision once.
    #[arg(long)]
    pub expected_revision: Option<u64>,
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
    #[arg(long = "instance-seed")]
    pub instance_seed: Option<String>,
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
