use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
/// Inspect and explicitly destroy provisioned resources.
pub enum ResourcesCommand {
    /// List one page of resources, or all resources with `--all`.
    List(ResourceListArgs),
    /// Inspect one resource.
    Inspect(ResourceInspectArgs),
    /// Destroy one detached resource after exact confirmation.
    Destroy(ResourceDestroyArgs),
}

#[derive(Debug, Args)]
pub struct ResourceListArgs {
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct ResourceInspectArgs {
    pub resource_id: String,
}

#[derive(Debug, Args)]
pub struct ResourceDestroyArgs {
    pub resource_id: String,
    #[arg(long)]
    pub expected_revision: u64,
    #[arg(long)]
    pub confirm_physical_id: String,
}
