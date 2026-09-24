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
    validate_nats_listener_ports, validate_output_dir, validate_required_nats_names,
    validate_required_trellis_options, validate_trellis_listener_collision,
};

/// Generate the NATS bootstrap output directory.
pub fn generate_nats_bootstrap(options: &NatsBootstrapOptions) -> Result<(), BootstrapError> {
    validate_required_nats_names(&options.config.names)?;
    validate_nats_listener_ports(&options.config)?;
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
        render_nats_config(
            &resolved_server_name(&options.config.names, trellis_name),
            options.config.nats_port,
            options.config.monitor_port,
            options.config.websocket_port,
        ),
    )?;
    let material = generate_nats_material(&options.config.names)?;
    write_nats_material(&options.out, &material)?;
    Ok(())
}

/// Generate a complete Trellis bootstrap bundle.
pub fn generate_trellis_bootstrap(options: &TrellisBootstrapOptions) -> Result<(), BootstrapError> {
    validate_required_trellis_options(options)?;
    validate_nats_listener_ports(&options.nats)?;
    validate_trellis_listener_collision(options)?;
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
    // The built-in live providers' identity seeds are provisioned material that
    // ships inside the (possibly read-only) bundle, so generate them here rather
    // than letting the runtime create them next to the session seed at startup.
    let live_providers = options.out.join("live-providers");
    create_private_dir(&live_providers)?;
    for key in ["platform", "health", "jobs", "events"] {
        write_private_file(
            live_providers.join(format!("{key}.seed")),
            format!("{}\n", trellis_local_bootstrap::generate_session_seed()),
        )?;
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), BootstrapError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn prepare_output_dir(out: &Path, force: bool) -> Result<(), BootstrapError> {
    validate_output_dir(out, force)?;
    if out.exists() && force {
        fs::remove_dir_all(out)?;
    }
    Ok(())
}
