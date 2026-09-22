use crate::app::{release_channel, SELF_UPDATE_TARGET};
use crate::cli::*;
use crate::output;
use crate::self_update::{check_for_update, install_update, UpdateResult};
pub(super) fn run_upgrade(format: OutputFormat, command: UpgradeCommand) -> miette::Result<()> {
    match command.command {
        UpgradeSubcommand::Check(args) => check_command(format, &args),
        UpgradeSubcommand::Install(args) => install_command(format, &args),
    }
}

fn check_command(format: OutputFormat, args: &UpgradeCheckArgs) -> miette::Result<()> {
    let check = check_for_update(SELF_UPDATE_TARGET, release_channel(args.prerelease))?;
    if output::is_json(format) {
        output::print_json(&check)?;
        return Ok(());
    }

    if check.needs_update {
        output::print_info(&format!(
            "update available: {} -> {}",
            check.current_version, check.latest_version
        ));
    } else {
        output::print_info(&format!("up to date: {}", check.current_version));
    }
    Ok(())
}

fn install_command(format: OutputFormat, args: &UpgradeInstallArgs) -> miette::Result<()> {
    let result = install_update(SELF_UPDATE_TARGET, release_channel(args.prerelease))?;
    if output::is_json(format) {
        output::print_json(&result)?;
        return Ok(());
    }

    match result {
        UpdateResult::UpToDate { version } => {
            output::print_info(&format!("up to date: {version}"));
        }
        UpdateResult::Updated { version } => {
            output::print_success(&format!("updated trellis to {version}"));
        }
    }
    Ok(())
}
