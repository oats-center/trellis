//! Generated application request, not a deployed participant.
/// Source-owned application request metadata.
pub struct Request;
impl trellis_rs::generated::AppRequestDescriptor for Request {
    const ID: &'static str = "trellis-test-fixture.OperationAuthorizationCaller";
    const KIND: trellis_rs::generated::AppKind = trellis_rs::generated::AppKind::Browser;
    const REQUIRED_CAPABILITIES: &'static [&'static str] = &[
        "trellis-test-fixture.echo@v1::optionalSilent",
    ];
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
    pub fn trellis_test_fixture_echo_v1(&self) -> trellis_test_fixture_echo_v1::Client {
        trellis_test_fixture_echo_v1::Client::from_generated(self.inner.clone())
    }
}
pub mod trellis_test_fixture_echo_v1 {
    use crate::apis::trellis_test_fixture_echo_v1::operations;
    #[derive(Clone)]
    pub struct Client {
        inner: trellis_rs::generated::Client,
    }
    impl Client {
        pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
            Self { inner }
        }
        pub fn optional_silent(
            &self,
        ) -> trellis_rs::generated::Operation<'_, operations::OptionalSilent> {
            self.inner.operation::<operations::OptionalSilent>()
        }
    }
}
pub mod types {
    //! Generated wire type exports.
}
