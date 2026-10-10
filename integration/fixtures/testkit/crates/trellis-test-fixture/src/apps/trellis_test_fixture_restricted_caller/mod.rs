//! Generated application request, not a deployed participant.
/// Source-owned application request metadata.
pub struct Request;
impl trellis_rs::generated::AppRequestDescriptor for Request {
    const ID: &'static str = "trellis-test-fixture.RestrictedCaller";
    const KIND: trellis_rs::generated::AppKind = trellis_rs::generated::AppKind::Browser;
    const REQUIRED_CAPABILITIES: &'static [&'static str] = &[];
    const OPTIONAL_CAPABILITIES: &'static [&'static str] = &[];
}
const OPTIONAL_ACTIONS: &[trellis_rs::generated::OptionalAction] = &[];
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self {
            inner: inner.with_optional_actions(OPTIONAL_ACTIONS),
        }
    }
}
pub mod types {
    //! Generated wire type exports.
}
