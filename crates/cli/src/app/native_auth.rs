//! Selected native credential identity and initial installation pin. Secrets and
//! authenticated successor pins belong to the SDK's existing OAuth custody.
use miette::IntoDiagnostic;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use trellis_rs::{
    auth::{NativeCredentialScope, NativeOAuthClient},
    client::RuntimeAuthorizationTrust,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    pub scope: NativeCredentialScope,
    pub nats_servers: Vec<String>,
    pub issuer_key: trellis_protocol::AuthorityIssuerKey,
    pub rotation_sequence: u64,
}

fn directory() -> miette::Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            miette::miette!("HOME or XDG_CONFIG_HOME is required for native OAuth custody")
        })?;
    if !base.is_absolute() {
        return Err(miette::miette!(
            "native OAuth configuration directory must be absolute"
        ));
    }
    Ok(base.join("trellis/oauth"))
}

impl Selection {
    pub async fn open(&self) -> miette::Result<NativeOAuthClient> {
        NativeOAuthClient::open(
            directory()?,
            self.scope.clone(),
            RuntimeAuthorizationTrust {
                instance_id: self.scope.instance_id.clone(),
                account: self.scope.account.clone(),
                issuer: self.issuer_key.clone(),
                rotation_sequence: self.rotation_sequence,
            },
        )
        .await
        .into_diagnostic()
    }

    pub fn save(&self) -> miette::Result<()> {
        use std::io::Write as _;
        // NativeOAuthClient::open has already established owner-only custody.
        let directory = directory()?;
        let mut staged = tempfile::NamedTempFile::new_in(&directory).into_diagnostic()?;
        staged
            .write_all(&serde_json::to_vec(self).into_diagnostic()?)
            .into_diagnostic()?;
        staged.as_file().sync_all().into_diagnostic()?;
        staged
            .persist(directory.join("selected.json"))
            .map_err(|error| error.error)
            .into_diagnostic()?;
        std::fs::File::open(directory)
            .into_diagnostic()?
            .sync_all()
            .into_diagnostic()?;
        Ok(())
    }

    pub fn load() -> miette::Result<Self> {
        let path = directory()?.join("selected.json");
        let metadata = std::fs::symlink_metadata(&path).into_diagnostic()?;
        if !metadata.is_file() {
            return Err(miette::miette!(
                "native OAuth selection must be a regular file"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
                return Err(miette::miette!("native OAuth selection must be owner-only"));
            }
        }
        serde_json::from_slice(&std::fs::read(path).into_diagnostic()?).into_diagnostic()
    }

    pub fn clear() -> miette::Result<bool> {
        let directory = directory()?;
        match std::fs::remove_file(directory.join("selected.json")) {
            Ok(()) => {
                std::fs::File::open(directory)
                    .into_diagnostic()?
                    .sync_all()
                    .into_diagnostic()?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).into_diagnostic(),
        }
    }
}
