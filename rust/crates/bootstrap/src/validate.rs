use std::fs;
use std::path::Path;

use crate::error::BootstrapError;
use crate::types::{NatsBootstrapNames, TrellisBootstrapOptions};

/// Validate whether an output directory can be used for generation.
pub fn validate_output_dir(out: &Path, force: bool) -> Result<(), BootstrapError> {
    if !out.exists() || force {
        return Ok(());
    }

    if fs::read_dir(out)?.next().is_some() {
        return Err(BootstrapError::OutputDirectoryNotEmpty {
            path: out.to_path_buf(),
        });
    }
    Ok(())
}

pub(crate) fn validate_required_nats_names(
    names: &NatsBootstrapNames,
) -> Result<(), BootstrapError> {
    if names.operator_name.trim().is_empty() {
        return Err(BootstrapError::MissingRequiredOption("operator_name"));
    }
    validate_generated_text_value("operator_name", &names.operator_name)?;
    if names.system_account.trim().is_empty() {
        return Err(BootstrapError::MissingRequiredOption("system_account"));
    }
    validate_generated_text_value("system_account", &names.system_account)?;
    if names.auth_account.trim().is_empty() {
        return Err(BootstrapError::MissingRequiredOption("auth_account"));
    }
    validate_generated_text_value("auth_account", &names.auth_account)?;
    if names.trellis_account.trim().is_empty() {
        return Err(BootstrapError::MissingRequiredOption("trellis_account"));
    }
    validate_generated_text_value("trellis_account", &names.trellis_account)?;
    if matches!(&names.server_name, Some(server_name) if server_name.trim().is_empty()) {
        return Err(BootstrapError::MissingRequiredOption("server_name"));
    }
    if let Some(server_name) = &names.server_name {
        validate_generated_text_value("server_name", server_name)?;
    }
    Ok(())
}

fn validate_generated_text_value(label: &'static str, value: &str) -> Result<(), BootstrapError> {
    if value.chars().any(char::is_control) {
        return Err(BootstrapError::InvalidGeneratedTextValue(label));
    }
    Ok(())
}

pub(crate) fn validate_required_trellis_options(
    options: &TrellisBootstrapOptions,
) -> Result<(), BootstrapError> {
    validate_required_nats_names(&options.nats.names)?;
    if options.runtime.name.trim().is_empty() {
        return Err(BootstrapError::MissingRequiredOption("name"));
    }
    Ok(())
}
