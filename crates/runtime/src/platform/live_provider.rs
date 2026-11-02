//! Built-in provider owner delivery and read-only provisioned identity custody.
//!
//! Fresh bootstrap writes provider seeds. Runtime startup never generates or
//! replaces them; native admission and store provisioning own provider authority.

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

/// Read a provisioned identity from `secrets.toml` without creating or
/// rotating it.
///
/// # Errors
/// A missing or malformed seed fails startup.
pub fn load_live_provider_seed(
    config: &crate::RuntimeConfig,
    role: LiveProviderRole,
) -> Result<String, RuntimeError> {
    let seeds = &config.secrets.live_providers;
    let (seed, name) = match role {
        LiveProviderRole::Platform => (seeds.platform.as_ref(), "live_providers.platform"),
        LiveProviderRole::Health => (seeds.health.as_ref(), "live_providers.health"),
        LiveProviderRole::Jobs => (seeds.jobs.as_ref(), "live_providers.jobs"),
        LiveProviderRole::Events => (seeds.events.as_ref(), "live_providers.events"),
    };
    let seed = crate::config::secrets_require(seed, name)?;
    SessionAuth::from_seed_base64url(seed).map_err(|error| {
        RuntimeError::Platform(format!("invalid {name} in secrets.toml: {error}"))
    })?;
    Ok(seed.to_owned())
}
