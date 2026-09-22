use clap::{Args, Subcommand};

#[derive(Debug, Args)]
/// Generate or derive Trellis keys.
pub struct KeysCommand {
    #[command(subcommand)]
    pub command: KeysSubcommand,
}

#[derive(Debug, Subcommand)]
/// Trellis key operations.
pub enum KeysSubcommand {
    /// Generate an Ed25519 seed and public session key offline.
    New(crate::cli::KeygenArgs),
}

#[derive(Debug, Args)]
/// Check for or install newer Trellis CLI releases.
pub struct UpgradeCommand {
    #[command(subcommand)]
    pub command: UpgradeSubcommand,
}

#[derive(Debug, Subcommand)]
/// Trellis CLI upgrade commands.
pub enum UpgradeSubcommand {
    /// Check GitHub releases and report whether an update is available.
    Check(UpgradeCheckArgs),
    /// Download and install the latest Trellis CLI release for this platform.
    Install(UpgradeInstallArgs),
}

#[derive(Debug, Args)]
/// Check whether a newer Trellis CLI release exists.
pub struct UpgradeCheckArgs {
    #[arg(long)]
    /// Include prerelease versions such as release candidates.
    pub prerelease: bool,
}

#[derive(Debug, Args)]
/// Install the newest Trellis CLI release for this platform.
pub struct UpgradeInstallArgs {
    #[arg(long)]
    /// Allow prerelease versions such as release candidates.
    pub prerelease: bool,
}
