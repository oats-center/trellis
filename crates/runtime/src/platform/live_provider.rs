//! Built-in provider owner delivery and read-only provisioned identity custody.
//!
//! Fresh bootstrap writes provider seeds. Runtime startup never generates or
//! replaces them; native admission and store provisioning own provider authority.

use std::path::PathBuf;
use trellis_rs::__internal::RuntimeOwner as _;
use trellis_rs::client::SessionAuth;

use super::RuntimeError;

/// Built-in roles that serve a live observation surface.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LiveProviderRole {
    /// Platform Auth/Core/State provider.
    Platform,
    /// Health API provider.
    Health,
    /// Jobs API provider.
    Jobs,
    /// Events API provider.
    Events,
}

impl LiveProviderRole {
    /// Configuration key for this role's provisioned seed.
    #[must_use]
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Health => "health",
            Self::Jobs => "jobs",
            Self::Events => "events",
        }
    }

    /// All built-in owner delivery roles.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Platform, Self::Health, Self::Jobs, Self::Events]
    }

    /// Roles whose subsystems run in the selected mode.
    #[must_use]
    pub fn roles_for_mode(mode: crate::RuntimeMode) -> Vec<Self> {
        mode.subsystems()
            .iter()
            .map(|subsystem| match subsystem {
                crate::SubsystemName::Platform => Self::Platform,
                crate::SubsystemName::Health => Self::Health,
                crate::SubsystemName::Jobs => Self::Jobs,
                crate::SubsystemName::Events => Self::Events,
            })
            .collect()
    }
}

type OwnerSlot = Option<trellis_rs::__internal::LiveProviderOwner>;

/// Delivery slots for independently supervised native provider owners.
#[derive(Clone)]
pub(crate) struct LiveProviderSlots {
    senders: std::sync::Arc<
        std::collections::BTreeMap<LiveProviderRole, tokio::sync::watch::Sender<OwnerSlot>>,
    >,
}

impl std::fmt::Debug for LiveProviderSlots {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveProviderSlots")
            .finish_non_exhaustive()
    }
}

impl LiveProviderSlots {
    pub(crate) fn new() -> Self {
        let senders = LiveProviderRole::all()
            .into_iter()
            .map(|role| {
                let (sender, _) = tokio::sync::watch::channel(None);
                (role, sender)
            })
            .collect();
        Self {
            senders: std::sync::Arc::new(senders),
        }
    }

    pub(crate) fn receiver(
        &self,
        role: LiveProviderRole,
    ) -> tokio::sync::watch::Receiver<OwnerSlot> {
        self.senders
            .get(&role)
            .expect("every live provider role has a delivery slot")
            .subscribe()
    }

    pub(crate) fn install(
        &self,
        role: LiveProviderRole,
        owner: trellis_rs::__internal::LiveProviderOwner,
    ) {
        if let Some(sender) = self.senders.get(&role) {
            let _ = sender.send(Some(owner));
        }
    }

    pub(crate) fn abort(&self) {
        for sender in self.senders.values() {
            if let Some(owner) = sender.send_replace(None) {
                owner.abort_native_runtime();
            }
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), RuntimeError> {
        let owners: Vec<_> = self
            .senders
            .values()
            .filter_map(|sender| sender.send_replace(None))
            .collect();
        let results = futures_util::future::join_all(
            owners
                .iter()
                .map(trellis_rs::__internal::LiveProviderOwner::shutdown_native_runtime),
        )
        .await;
        for result in results {
            result.map_err(|error| RuntimeError::Platform(error.to_string()))?;
        }
        Ok(())
    }
}

/// Wait for a supervised native owner, or return when the host shuts down.
pub(crate) async fn await_live_owner(
    receiver: &mut tokio::sync::watch::Receiver<OwnerSlot>,
    stop: &crate::shutdown::StopHandle,
) -> Option<trellis_rs::__internal::LiveProviderOwner> {
    loop {
        if let Some(owner) = receiver.borrow_and_update().clone() {
            return Some(owner);
        }
        tokio::select! {
            changed = receiver.changed() => { if changed.is_err() { return None; } }
            () = stop.stopped() => return None,
        }
    }
}

/// Read a provisioned identity without creating or rotating it.
///
/// # Errors
/// Missing, unsafe, unreadable, or malformed seed files fail startup.
pub fn load_live_provider_seed(
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
) -> Result<String, RuntimeError> {
    use std::io::Read as _;
    let path = resolve_live_provider_seed_file(config, role)?;
    let read = || -> Result<String, std::io::Error> {
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "provider seed must be a regular file",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options.open(&path)?;
        let opened = file.metadata()?;
        if !opened.is_file() || opened.len() > 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid provider seed file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            // SAFETY: geteuid has no arguments or borrowed memory.
            let owner = unsafe { libc::geteuid() };
            if metadata.dev() != opened.dev()
                || metadata.ino() != opened.ino()
                || opened.mode() & 0o077 != 0
                || opened.uid() != owner
                || opened.nlink() != 1
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "provider seed must be owner-only",
                ));
            }
        }
        let mut contents = String::new();
        file.take(1024).read_to_string(&mut contents)?;
        Ok(contents.trim().to_owned())
    };
    let seed = read().map_err(|error| {
        RuntimeError::Platform(format!(
            "failed to read live provider seed '{}': {error}",
            path.display()
        ))
    })?;
    SessionAuth::from_seed_base64url(&seed).map_err(|error| {
        RuntimeError::Platform(format!(
            "invalid live provider seed '{}': {error}",
            path.display()
        ))
    })?;
    Ok(seed)
}

/// Resolve the configured path or the existing event-seed-relative default.
///
/// # Errors
/// An unconfigured role requires an event seed with a usable parent directory.
pub fn resolve_live_provider_seed_file(
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
) -> Result<PathBuf, RuntimeError> {
    let explicit = config
        .live_provider_seed_files
        .as_ref()
        .and_then(|files| match role {
            LiveProviderRole::Platform => files.platform.clone(),
            LiveProviderRole::Health => files.health.clone(),
            LiveProviderRole::Jobs => files.jobs.clone(),
            LiveProviderRole::Events => files.events.clone(),
        });
    if let Some(path) = explicit {
        return Ok(path);
    }
    let parent = config
        .event_session_seed_file
        .as_ref()
        .and_then(|path| path.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            RuntimeError::Platform(format!(
                "live_provider_seed_files.{} is required when the event session seed \
                file has no parent directory",
                role.config_key()
            ))
        })?;
    Ok(parent
        .join("live-providers")
        .join(format!("{}.seed", role.config_key())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_provider_seeds_authenticate_and_missing_or_corrupt_custody_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let config: crate::RuntimeConfig = serde_json::from_value(
            serde_json::json!({"event_session_seed_file":directory.path().join("session.seed")}),
        )
        .unwrap();
        let path = resolve_live_provider_seed_file(&config, LiveProviderRole::Platform).unwrap();
        assert!(load_live_provider_seed(&config, LiveProviderRole::Platform).is_err());
        assert!(!path.exists());
        trellis_local_bootstrap::generate_local_live_provider_seeds(
            &directory.path().join("live-providers"),
        )
        .unwrap();
        let seed = load_live_provider_seed(&config, LiveProviderRole::Platform).unwrap();
        let key = SessionAuth::from_seed_base64url(&seed).unwrap();
        let proof = key.sign_sha256_bytes(b"provider challenge");
        let reopened = SessionAuth::from_seed_base64url(
            &load_live_provider_seed(&config, LiveProviderRole::Platform).unwrap(),
        )
        .unwrap();
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use sha2::{Digest as _, Sha256};
        let public: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&reopened.session_key)
            .unwrap()
            .try_into()
            .unwrap();
        let signature =
            ed25519_dalek::Signature::from_slice(&URL_SAFE_NO_PAD.decode(proof).unwrap()).unwrap();
        ed25519_dalek::VerifyingKey::from_bytes(&public)
            .unwrap()
            .verify_strict(&Sha256::digest(b"provider challenge"), &signature)
            .unwrap();
        assert!(trellis_local_bootstrap::generate_local_live_provider_seeds(
            &directory.path().join("live-providers")
        )
        .is_err());
        assert_eq!(
            load_live_provider_seed(&config, LiveProviderRole::Platform).unwrap(),
            seed
        );
        std::fs::write(&path, "invalid seed").unwrap();
        assert!(load_live_provider_seed(&config, LiveProviderRole::Platform).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid seed");
        std::fs::remove_file(&path).unwrap();
        assert!(load_live_provider_seed(&config, LiveProviderRole::Platform).is_err());
        assert!(!path.exists());
    }
}
