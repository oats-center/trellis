use std::fs;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use crate::error::BootstrapError;
use crate::nats_material::{render_user_creds, NatsMaterial};
pub(crate) fn create_layout(out: &Path) -> Result<(), BootstrapError> {
    fs::create_dir_all(out.join("creds"))?;
    fs::create_dir_all(out.join("secrets"))?;
    set_private_dir_permissions(&out.join("creds"))?;
    set_private_dir_permissions(&out.join("secrets"))?;
    Ok(())
}

fn set_private_dir_permissions(path: &Path) -> Result<(), BootstrapError> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Writes a NATS-only bundle's user creds and auth-callout keys as files.
pub(crate) fn write_nats_material(
    out: &Path,
    material: &NatsMaterial,
) -> Result<(), BootstrapError> {
    write_private_file(
        out.join("creds/system.creds"),
        render_user_creds(&material.system_user_jwt, &material.system_user_seed),
    )?;
    write_private_file(
        out.join("creds/auth-auth.creds"),
        render_user_creds(&material.auth_user_jwt, &material.auth_user_seed),
    )?;
    write_private_file(
        out.join("creds/trellis-auth.creds"),
        render_user_creds(&material.trellis_user_jwt, &material.trellis_user_seed),
    )?;
    write_private_file(
        out.join("secrets/auth-issuer-signing.seed"),
        &material.auth_issuer_signing_seed,
    )?;
    write_private_file(
        out.join("secrets/auth-target-signing.seed"),
        &material.auth_target_signing_seed,
    )?;
    write_private_file(
        out.join("secrets/auth-sx.seed"),
        &material.auth_callout_xkey_seed,
    )?;
    fs::write(
        out.join("secrets/auth-sentinel.pub"),
        &material.metadata.auth_sentinel_public_key,
    )?;
    Ok(())
}

pub(crate) fn write_private_file(
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
) -> Result<(), BootstrapError> {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_ref())?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        fs::write(path, contents)?;
        Ok(())
    }
}
