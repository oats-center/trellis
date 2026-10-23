//! Private key material written once when a Trellis installation is initialized.
//!
//! Every file is created exclusively with owner-only permissions; existing
//! material is never replaced.

use std::fs;
use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

/// Generate a fresh base64url-encoded 32-byte session seed for the event log.
///
/// Shared with `trellis-bootstrap` so both bootstrap generators write byte-identical seeds.
#[must_use]
pub fn generate_session_seed() -> String {
    let seed: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(seed)
}

/// Provision built-in provider identities during fresh initialization only.
/// Existing files are never replaced; normal runtime startup only reads them.
pub fn generate_local_live_provider_seeds(directory: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    match builder.create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "provider seed directory must be a real directory",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "provider seed directory must be private",
                    ));
                }
            }
        }
        Err(error) => return Err(error),
    }
    for role in ["platform", "health", "jobs", "events"] {
        write_secret(
            &directory.join(format!("{role}.seed")),
            &generate_session_seed(),
        )?;
    }
    Ok(())
}

/// Generate a private online authorization-context issuer seed without trust files.
pub fn generate_local_authorization_issuer(directory: &Path) -> std::io::Result<()> {
    fs::create_dir_all(directory)?;
    write_secret(
        &directory.join("authorization-issuer.seed"),
        &generate_session_seed(),
    )
}

fn write_secret(path: &Path, value: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    writeln!(options.open(path)?, "{value}")?;
    Ok(())
}

/// Provision an installation-owned OAuth storage key without replacing existing material.
/// This key is independent of online signing keys and must survive issuer rotation.
pub fn generate_local_oauth_storage_key(directory: &Path) -> std::io::Result<()> {
    write_secret(
        &directory.join("oauth-storage.key"),
        &generate_session_seed(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn fresh_provider_custody_refuses_replacement_and_unsafe_directory() {
        let directory = tempfile::tempdir().unwrap();
        let providers = directory.path().join("live-providers");
        super::generate_local_live_provider_seeds(&providers).unwrap();
        let platform = providers.join("platform.seed");
        let saved = std::fs::read(&platform).unwrap();
        assert!(super::generate_local_live_provider_seeds(&providers).is_err());
        assert_eq!(std::fs::read(&platform).unwrap(), saved);
        std::fs::write(&platform, "corrupt custody").unwrap();
        assert!(super::generate_local_live_provider_seeds(&providers).is_err());
        assert_eq!(
            std::fs::read_to_string(&platform).unwrap(),
            "corrupt custody"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt as _};
            let linked = directory.path().join("linked");
            symlink(&providers, &linked).unwrap();
            assert!(super::generate_local_live_provider_seeds(&linked).is_err());
            let exposed = directory.path().join("exposed");
            std::fs::create_dir(&exposed).unwrap();
            std::fs::set_permissions(&exposed, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(super::generate_local_live_provider_seeds(&exposed).is_err());
            assert!(!exposed.join("platform.seed").exists());
        }
    }

    #[test]
    fn generated_session_seed_is_base64url_encoded_32_bytes() {
        let seed = generate_session_seed();
        let decoded = URL_SAFE_NO_PAD.decode(seed).expect("decode session seed");

        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn local_authorization_issuer_is_private_and_never_overwritten() {
        let directory = tempfile::tempdir().expect("temp directory");
        generate_local_authorization_issuer(directory.path()).expect("generate issuer");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("read trust directory")
                .map(|entry| entry.expect("read trust entry").file_name())
                .collect::<std::collections::BTreeSet<_>>(),
            ["authorization-issuer.seed"]
                .into_iter()
                .map(OsString::from)
                .collect()
        );
        let path = directory.path().join("authorization-issuer.seed");
        let seed = fs::read_to_string(&path).expect("read issuer seed");
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(seed.trim())
                .expect("decode seed")
                .len(),
            32
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path)
                    .expect("seed metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(generate_local_authorization_issuer(directory.path()).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read retained seed"), seed);
    }
}
