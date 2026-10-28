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
/// Install the newest Trellis CLI release for this platform.
pub struct UpgradeCommand {
    #[arg(long)]
    /// Only report whether a newer release exists.
    pub check: bool,
    #[arg(long)]
    /// Include prerelease versions such as release candidates.
    pub prerelease: bool,
}
