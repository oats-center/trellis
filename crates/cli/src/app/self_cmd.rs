use crate::app::{release_channel, SELF_UPDATE_TARGET};
use crate::cli::*;
use crate::output;
use trellis_self_update::{check_for_update, install_update, ReleaseChannel, UpdateResult};
pub(super) fn run_upgrade(format: OutputFormat, command: UpgradeCommand) -> miette::Result<()> {
    let channel = release_channel(command.prerelease);
    if !command.check {
        return install_command(format, channel);
    }
    let check = check_for_update(SELF_UPDATE_TARGET, channel)?;
    if output::is_json(format) {
        output::print_json(&check)?;
        return Ok(());
    }

    if check.needs_update {
        output::note(format!(
            "{} {} is available (this is {})",
            output::strong("trellis"),
            output::strong(&check.latest_version),
            check.current_version
        ));
        output::next(if command.prerelease {
            "trellis upgrade --prerelease"
        } else {
            "trellis upgrade"
        });
    } else {
        output::unchanged(format!("trellis {} is up to date", check.current_version));
    }
    Ok(())
}

fn install_command(format: OutputFormat, channel: ReleaseChannel) -> miette::Result<()> {
    let result = install_update(SELF_UPDATE_TARGET, channel)?;
    if output::is_json(format) {
        output::print_json(&result)?;
        return Ok(());
    }

    match result {
        UpdateResult::UpToDate { version } => {
            output::unchanged(format!("trellis {version} is up to date"));
        }
        UpdateResult::Updated { version } => {
            output::success(format!("Updated trellis to {}", output::strong(version)));
        }
    }
    Ok(())
}
