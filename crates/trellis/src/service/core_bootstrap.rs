use super::{BootstrapBinding, BootstrapBindingInfo, ServiceResourceBindings};

/// Runtime binding returned by the authenticated bootstrap endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreBootstrapBinding {
    binding: BootstrapBinding,
    resources: ServiceResourceBindings,
}

impl CoreBootstrapBinding {
    /// Construct a bootstrap binding from validated session and resource evidence.
    pub fn new(binding: BootstrapBinding, resources: ServiceResourceBindings) -> Self {
        Self { binding, resources }
    }

    pub(crate) fn jobs_runtime_binding(
        &self,
    ) -> Result<crate::jobs::JobsRuntimeBinding, crate::jobs::bindings::JobsBindingError> {
        crate::jobs::JobsRuntimeBinding::try_from(&self.resources)
    }
}

impl BootstrapBindingInfo for CoreBootstrapBinding {
    fn bootstrap_binding(&self) -> BootstrapBinding {
        self.binding.clone()
    }

    fn resource_bindings(&self) -> ServiceResourceBindings {
        self.resources.clone()
    }
}
