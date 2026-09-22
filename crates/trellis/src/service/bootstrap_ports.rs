use super::{BootstrapBinding, ServiceResourceBindings};

/// A resolved service binding that can expose the validated contract id/digest pair.
pub trait BootstrapBindingInfo: Clone + Send + Sync {
    fn bootstrap_binding(&self) -> BootstrapBinding;

    /// Return typed resource bindings resolved for this bootstrap binding.
    fn resource_bindings(&self) -> ServiceResourceBindings {
        ServiceResourceBindings::default()
    }
}

impl BootstrapBindingInfo for BootstrapBinding {
    fn bootstrap_binding(&self) -> BootstrapBinding {
        self.clone()
    }
}
