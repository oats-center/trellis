use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{AdminSessionState, TrellisAuthError};

fn cli_config_dir() -> PathBuf {
    if let Ok(dir) = env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("trellis");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".config/trellis");
    }
    PathBuf::from(".trellis")
}

fn admin_session_state_path() -> PathBuf {
    cli_config_dir().join("admin-session.json")
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), TrellisAuthError> {
    if let Some(parent) = path.parent() {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(parent)?;
    }
    let staged = path.with_extension(format!("{}.tmp", ulid::Ulid::new()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&staged)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&staged, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&staged) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "cannot remove staged login credentials");
            }
        }
    }
    Ok(result?)
}

/// Persist an admin session to the CLI config directory.
#[doc = concat!("Trellis API operation `", stringify!(save_admin_session), "`.")]
pub fn save_admin_session(state: &AdminSessionState) -> Result<(), TrellisAuthError> {
    let state_json = serde_json::to_string_pretty(state)?;
    write_private_file(&admin_session_state_path(), &state_json)
}

/// Load the current admin session from disk.
#[doc = concat!("Trellis API operation `", stringify!(load_admin_session), "`.")]
pub fn load_admin_session() -> Result<AdminSessionState, TrellisAuthError> {
    let path = admin_session_state_path();
    if !path.exists() {
        return Err(TrellisAuthError::AuthFlowFailed(
            "no stored admin session; run `trellis auth login`".to_string(),
        ));
    }
    let state = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&state)?)
}

/// Remove the stored admin login credentials.
#[doc = concat!("Trellis API operation `", stringify!(clear_admin_session), "`.")]
pub fn clear_admin_session() -> Result<bool, TrellisAuthError> {
    let path = admin_session_state_path();
    if path.exists() {
        fs::remove_file(path)?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_replacement_and_failed_commit_use_real_filesystem() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        write_private_file(&path, "first").unwrap();
        write_private_file(&path, "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let occupied = directory.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("keep"), "retained").unwrap();
        assert!(write_private_file(&occupied, "replacement").is_err());
        assert_eq!(
            fs::read_to_string(occupied.join("keep")).unwrap(),
            "retained"
        );
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}
