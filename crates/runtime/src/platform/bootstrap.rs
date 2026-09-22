//! Platform bootstrap scaffold.

/// Runtime state for service and device bootstrap flows.
///
/// Bootstrap checks presented contracts against deployment authority and waits
/// for materialized authority before returning scoped credentials and resolved
/// runtime bindings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bootstrap;
