use std::fs;
use std::path::Path;

use crate::constants::DEFAULT_TRELLIS_NAME;
use crate::error::BootstrapError;
use crate::nats_config::{render_nats_config, resolved_server_name};
use crate::nats_material::generate_nats_material;
use crate::output::{create_layout, write_nats_material, write_private_file};
use crate::runtime_config::render_trellis_config;
use crate::types::{NatsBootstrapOptions, TrellisBootstrapOptions};
use crate::validate::{
    validate_output_dir, validate_required_nats_names, validate_required_trellis_options,
};

/// Generate the NATS bootstrap output directory.
pub fn generate_nats_bootstrap(options: &NatsBootstrapOptions) -> Result<(), BootstrapError> {
    validate_required_nats_names(&options.config.names)?;
    prepare_output_dir(&options.out, options.force)?;

    generate_nats_bootstrap_inner(options, DEFAULT_TRELLIS_NAME)
}

fn generate_nats_bootstrap_inner(
    options: &NatsBootstrapOptions,
    trellis_name: &str,
) -> Result<(), BootstrapError> {
    create_layout(&options.out)?;
    fs::write(
        options.out.join("nats.conf"),
        render_nats_config(&resolved_server_name(&options.config.names, trellis_name)),
    )?;
    let material = generate_nats_material(&options.config.names)?;
    write_nats_material(&options.out, &material)?;
    Ok(())
}

/// Generate a complete Trellis bootstrap bundle.
pub fn generate_trellis_bootstrap(options: &TrellisBootstrapOptions) -> Result<(), BootstrapError> {
    validate_required_trellis_options(options)?;
    prepare_output_dir(&options.out, options.force)?;

    let nats_out = options.out.join("nats");
    let trellis_out = &options.out;
    let mut nats_options = NatsBootstrapOptions::new(&nats_out);
    nats_options.force = false;
    nats_options.config = options.nats.clone();

    generate_nats_bootstrap_inner(&nats_options, &options.runtime.name)?;
    let authorization_directory = trellis_out.join("auth");
    trellis_local_bootstrap::generate_local_authorization_issuer(&authorization_directory)
        .map_err(|error| BootstrapError::AuthorizationTrust(error.to_string()))?;
    fs::write(
        trellis_out.join("config.toml"),
        render_trellis_config(options),
    )?;
    write_private_file(
        options.out.join("session.seed"),
        format!("{}\n", trellis_local_bootstrap::generate_session_seed()),
    )?;
    Ok(())
}

fn prepare_output_dir(out: &Path, force: bool) -> Result<(), BootstrapError> {
    validate_output_dir(out, force)?;
    if out.exists() && force {
        fs::remove_dir_all(out)?;
    }
    Ok(())
}
