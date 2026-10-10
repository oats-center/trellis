//! Generated API `trellis.auth@v1`.
pub const API_ID: &str = "trellis.auth@v1";
pub const API_DIGEST: &str = "CF6mJ1tZZXKP6VWFqfKo3ey8wHhhcyNPEFybZ53NFPk";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct AuthError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl AuthError {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::AuthErrorDetails, serde_json::Error> {
            let mut payload = self.error.extra.clone();
            payload.insert(
                "id".to_owned(),
                serde_json::Value::String(self.error.id.clone()),
            );
            payload.insert(
                "type".to_owned(),
                serde_json::Value::String(self.error.error_type.clone()),
            );
            payload.insert(
                "message".to_owned(),
                serde_json::Value::String(self.error.message.clone()),
            );
            if let Some(context) = &self.error.context {
                payload.insert(
                    "context".to_owned(),
                    serde_json::Value::Object(context.clone()),
                );
            }
            if let Some(trace_id) = &self.error.trace_id {
                payload.insert(
                    "traceId".to_owned(),
                    serde_json::Value::String(trace_id.clone()),
                );
            }
            serde_json::from_value(serde_json::Value::Object(payload))
        }
    }
    impl std::fmt::Display for AuthError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for AuthError {}
    impl trellis_rs::generated::TrellisError for AuthError {
        const TYPE: &'static str = "trellis.auth@v1::AuthError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UnexpectedError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl UnexpectedError {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::AuthErrorDetails, serde_json::Error> {
            let mut payload = self.error.extra.clone();
            payload.insert(
                "id".to_owned(),
                serde_json::Value::String(self.error.id.clone()),
            );
            payload.insert(
                "type".to_owned(),
                serde_json::Value::String(self.error.error_type.clone()),
            );
            payload.insert(
                "message".to_owned(),
                serde_json::Value::String(self.error.message.clone()),
            );
            if let Some(context) = &self.error.context {
                payload.insert(
                    "context".to_owned(),
                    serde_json::Value::Object(context.clone()),
                );
            }
            if let Some(trace_id) = &self.error.trace_id {
                payload.insert(
                    "traceId".to_owned(),
                    serde_json::Value::String(trace_id.clone()),
                );
            }
            serde_json::from_value(serde_json::Value::Object(payload))
        }
    }
    impl std::fmt::Display for UnexpectedError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for UnexpectedError {}
    impl trellis_rs::generated::TrellisError for UnexpectedError {
        const TYPE: &'static str = "trellis.auth@v1::UnexpectedError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct ValidationError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl ValidationError {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::AuthErrorDetails, serde_json::Error> {
            let mut payload = self.error.extra.clone();
            payload.insert(
                "id".to_owned(),
                serde_json::Value::String(self.error.id.clone()),
            );
            payload.insert(
                "type".to_owned(),
                serde_json::Value::String(self.error.error_type.clone()),
            );
            payload.insert(
                "message".to_owned(),
                serde_json::Value::String(self.error.message.clone()),
            );
            if let Some(context) = &self.error.context {
                payload.insert(
                    "context".to_owned(),
                    serde_json::Value::Object(context.clone()),
                );
            }
            if let Some(trace_id) = &self.error.trace_id {
                payload.insert(
                    "traceId".to_owned(),
                    serde_json::Value::String(trace_id.clone()),
                );
            }
            serde_json::from_value(serde_json::Value::Object(payload))
        }
    }
    impl std::fmt::Display for ValidationError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for ValidationError {}
    impl trellis_rs::generated::TrellisError for ValidationError {
        const TYPE: &'static str = "trellis.auth@v1::ValidationError";
    }
}
pub mod rpc {
    pub type AdmissionRenewInput = crate::__types::trellis::AuthAdmissionRenewRequest;
    pub type AdmissionRenewOutput = crate::__types::trellis::AuthAdmissionRenewResponse;
    pub struct AdmissionRenew;
    impl AdmissionRenew {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Admission.Renew";
        pub const KEY: &'static str = "auth.Admission.Renew";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Admission.Renew";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum AdmissionRenewError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl AdmissionRenewError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for AdmissionRenew {
        type Input = AdmissionRenewInput;
        type Output = AdmissionRenewOutput;
        type Error = AdmissionRenewError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            AdmissionRenewError::decode(value)
        }
    }
    pub type ApisAcceptInput = crate::__types::trellis::AuthApiAcceptRequest;
    pub type ApisAcceptOutput = crate::__types::trellis::AuthApiAcceptResponse;
    pub struct ApisAccept;
    impl ApisAccept {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Apis.Accept";
        pub const KEY: &'static str = "auth.Apis.Accept";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Apis.Accept";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::apisAccept"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ApisAcceptError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ApisAcceptError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ApisAccept {
        type Input = ApisAcceptInput;
        type Output = ApisAcceptOutput;
        type Error = ApisAcceptError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ApisAcceptError::decode(value)
        }
    }
    pub type ApisForceReplaceInput = crate::__types::trellis::AuthApiForceReplaceRequest;
    pub type ApisForceReplaceOutput = crate::__types::trellis::AuthApiAcceptResponse;
    pub struct ApisForceReplace;
    impl ApisForceReplace {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Apis.ForceReplace";
        pub const KEY: &'static str = "auth.Apis.ForceReplace";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Apis.ForceReplace";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::apisForceReplace"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ApisForceReplaceError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ApisForceReplaceError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ApisForceReplace {
        type Input = ApisForceReplaceInput;
        type Output = ApisForceReplaceOutput;
        type Error = ApisForceReplaceError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ApisForceReplaceError::decode(value)
        }
    }
    pub type ApisGetInput = crate::__types::trellis::AuthApiGetRequest;
    pub type ApisGetOutput = crate::__types::trellis::AuthApiGetResponse;
    pub struct ApisGet;
    impl ApisGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Apis.Get";
        pub const KEY: &'static str = "auth.Apis.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Apis.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::apisRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ApisGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ApisGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ApisGet {
        type Input = ApisGetInput;
        type Output = ApisGetOutput;
        type Error = ApisGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ApisGetError::decode(value)
        }
    }
    pub type ApisListInput = crate::__types::trellis::AuthPageRequest;
    pub type ApisListOutput = crate::__types::trellis::AuthApisListResponse;
    pub struct ApisList;
    impl ApisList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Apis.List";
        pub const KEY: &'static str = "auth.Apis.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Apis.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::apisRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ApisListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ApisListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ApisList {
        type Input = ApisListInput;
        type Output = ApisListOutput;
        type Error = ApisListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ApisListError::decode(value)
        }
    }
    pub type ApisReviewInput = crate::__types::trellis::AuthApiReviewRequest;
    pub type ApisReviewOutput = crate::__types::trellis::AuthApiReviewResponse;
    pub struct ApisReview;
    impl ApisReview {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Apis.Review";
        pub const KEY: &'static str = "auth.Apis.Review";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Apis.Review";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::apisReview"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ApisReviewError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ApisReviewError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ApisReview {
        type Input = ApisReviewInput;
        type Output = ApisReviewOutput;
        type Error = ApisReviewError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ApisReviewError::decode(value)
        }
    }
    pub type AuthorityRenewInput = crate::__types::trellis::AuthAuthorityRenewRequest;
    pub type AuthorityRenewOutput = crate::__types::trellis::AuthAuthorityRenewResponse;
    pub struct AuthorityRenew;
    impl AuthorityRenew {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Authority.Renew";
        pub const KEY: &'static str = "auth.Authority.Renew";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Authority.Renew";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum AuthorityRenewError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl AuthorityRenewError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for AuthorityRenew {
        type Input = AuthorityRenewInput;
        type Output = AuthorityRenewOutput;
        type Error = AuthorityRenewError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            AuthorityRenewError::decode(value)
        }
    }
    pub type AuthorityResolveInput = crate::__types::trellis::AuthAuthorityResolveRequest;
    pub type AuthorityResolveOutput = crate::__types::trellis::AuthAuthorityResolveResponse;
    pub struct AuthorityResolve;
    impl AuthorityResolve {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Authority.Resolve";
        pub const KEY: &'static str = "auth.Authority.Resolve";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Authority.Resolve";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum AuthorityResolveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl AuthorityResolveError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for AuthorityResolve {
        type Input = AuthorityResolveInput;
        type Output = AuthorityResolveOutput;
        type Error = AuthorityResolveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            AuthorityResolveError::decode(value)
        }
    }
    pub type AuthorityStatusInput = crate::__types::trellis::AuthAuthorityStatusRequest;
    pub type AuthorityStatusOutput = crate::__types::trellis::AuthAuthorityStatusResponse;
    pub struct AuthorityStatus;
    impl AuthorityStatus {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Authority.Status";
        pub const KEY: &'static str = "auth.Authority.Status";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Authority.Status";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum AuthorityStatusError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl AuthorityStatusError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for AuthorityStatus {
        type Input = AuthorityStatusInput;
        type Output = AuthorityStatusOutput;
        type Error = AuthorityStatusError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            AuthorityStatusError::decode(value)
        }
    }
    pub type AuthorizationSessionsListInput =
        crate::__types::trellis::AuthAuthorizationSessionsListRequest;
    pub type AuthorizationSessionsListOutput =
        crate::__types::trellis::AuthAuthorizationSessionsListResponse;
    pub struct AuthorizationSessionsList;
    impl AuthorizationSessionsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.AuthorizationSessions.List";
        pub const KEY: &'static str = "auth.AuthorizationSessions.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.AuthorizationSessions.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorizationSessionsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum AuthorizationSessionsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl AuthorizationSessionsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for AuthorizationSessionsList {
        type Input = AuthorizationSessionsListInput;
        type Output = AuthorizationSessionsListOutput;
        type Error = AuthorizationSessionsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            AuthorizationSessionsListError::decode(value)
        }
    }
    pub type AuthorizationSessionsRevokeInput =
        crate::__types::trellis::AuthAuthorizationSessionRevokeRequest;
    pub type AuthorizationSessionsRevokeOutput =
        crate::__types::trellis::AuthAuthorizationSessionRevokeResponse;
    pub struct AuthorizationSessionsRevoke;
    impl AuthorizationSessionsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.AuthorizationSessions.Revoke";
        pub const KEY: &'static str = "auth.AuthorizationSessions.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.AuthorizationSessions.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorizationSessionsRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum AuthorizationSessionsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl AuthorizationSessionsRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for AuthorizationSessionsRevoke {
        type Input = AuthorizationSessionsRevokeInput;
        type Output = AuthorizationSessionsRevokeOutput;
        type Error = AuthorizationSessionsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            AuthorizationSessionsRevokeError::decode(value)
        }
    }
    pub type CapabilitiesListInput = crate::__types::trellis::AuthCapabilitiesListRequest;
    pub type CapabilitiesListOutput = crate::__types::trellis::AuthCapabilitiesListResponse;
    pub struct CapabilitiesList;
    impl CapabilitiesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Capabilities.List";
        pub const KEY: &'static str = "auth.Capabilities.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Capabilities.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilitiesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilitiesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilitiesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for CapabilitiesList {
        type Input = CapabilitiesListInput;
        type Output = CapabilitiesListOutput;
        type Error = CapabilitiesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilitiesListError::decode(value)
        }
    }
    pub type CapabilityGrantsGrantInput = crate::__types::trellis::AuthCapabilityGrantRequest;
    pub type CapabilityGrantsGrantOutput = crate::__types::trellis::AuthCapabilityGrantResponse;
    pub struct CapabilityGrantsGrant;
    impl CapabilityGrantsGrant {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGrants.Grant";
        pub const KEY: &'static str = "auth.CapabilityGrants.Grant";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGrants.Grant";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilityGrantsGrant"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGrantsGrantError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGrantsGrantError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for CapabilityGrantsGrant {
        type Input = CapabilityGrantsGrantInput;
        type Output = CapabilityGrantsGrantOutput;
        type Error = CapabilityGrantsGrantError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGrantsGrantError::decode(value)
        }
    }
    pub type CapabilityGrantsListInput = crate::__types::trellis::AuthCapabilityGrantsListRequest;
    pub type CapabilityGrantsListOutput = crate::__types::trellis::AuthCapabilityGrantsListResponse;
    pub struct CapabilityGrantsList;
    impl CapabilityGrantsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGrants.List";
        pub const KEY: &'static str = "auth.CapabilityGrants.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGrants.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilityGrantsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGrantsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGrantsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for CapabilityGrantsList {
        type Input = CapabilityGrantsListInput;
        type Output = CapabilityGrantsListOutput;
        type Error = CapabilityGrantsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGrantsListError::decode(value)
        }
    }
    pub type CapabilityGrantsRevokeInput = crate::__types::trellis::AuthCapabilityRevokeRequest;
    pub type CapabilityGrantsRevokeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct CapabilityGrantsRevoke;
    impl CapabilityGrantsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGrants.Revoke";
        pub const KEY: &'static str = "auth.CapabilityGrants.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGrants.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilityGrantsRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGrantsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGrantsRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for CapabilityGrantsRevoke {
        type Input = CapabilityGrantsRevokeInput;
        type Output = CapabilityGrantsRevokeOutput;
        type Error = CapabilityGrantsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGrantsRevokeError::decode(value)
        }
    }
    pub type ConnectionsKickInput = crate::__types::trellis::AuthConnectionsKickRequest;
    pub type ConnectionsKickOutput = crate::__types::trellis::AuthConnectionsKickResponse;
    pub struct ConnectionsKick;
    impl ConnectionsKick {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Connections.Kick";
        pub const KEY: &'static str = "auth.Connections.Kick";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Connections.Kick";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::connectionsKick"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ConnectionsKickError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ConnectionsKickError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ConnectionsKick {
        type Input = ConnectionsKickInput;
        type Output = ConnectionsKickOutput;
        type Error = ConnectionsKickError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ConnectionsKickError::decode(value)
        }
    }
    pub type ConnectionsListInput = crate::__types::trellis::AuthConnectionsListRequest;
    pub type ConnectionsListOutput = crate::__types::trellis::AuthConnectionsListResponse;
    pub struct ConnectionsList;
    impl ConnectionsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Connections.List";
        pub const KEY: &'static str = "auth.Connections.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Connections.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::connectionsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ConnectionsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ConnectionsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ConnectionsList {
        type Input = ConnectionsListInput;
        type Output = ConnectionsListOutput;
        type Error = ConnectionsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ConnectionsListError::decode(value)
        }
    }
    pub type DelegationsListInput = crate::__types::trellis::AuthDelegationsListRequest;
    pub type DelegationsListOutput = crate::__types::trellis::AuthDelegationsListResponse;
    pub struct DelegationsList;
    impl DelegationsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Delegations.List";
        pub const KEY: &'static str = "auth.Delegations.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Delegations.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::delegationsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DelegationsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DelegationsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DelegationsList {
        type Input = DelegationsListInput;
        type Output = DelegationsListOutput;
        type Error = DelegationsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DelegationsListError::decode(value)
        }
    }
    pub type DelegationsReviewInput = crate::__types::trellis::AuthDelegationReviewRequest;
    pub type DelegationsReviewOutput = crate::__types::trellis::AuthDelegationReviewResponse;
    pub struct DelegationsReview;
    impl DelegationsReview {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Delegations.Review";
        pub const KEY: &'static str = "auth.Delegations.Review";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Delegations.Review";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::delegationsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DelegationsReviewError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DelegationsReviewError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DelegationsReview {
        type Input = DelegationsReviewInput;
        type Output = DelegationsReviewOutput;
        type Error = DelegationsReviewError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DelegationsReviewError::decode(value)
        }
    }
    pub type DelegationsRevokeInput = crate::__types::trellis::AuthDelegationRevokeRequest;
    pub type DelegationsRevokeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct DelegationsRevoke;
    impl DelegationsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Delegations.Revoke";
        pub const KEY: &'static str = "auth.Delegations.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Delegations.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::delegationsRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DelegationsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DelegationsRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DelegationsRevoke {
        type Input = DelegationsRevokeInput;
        type Output = DelegationsRevokeOutput;
        type Error = DelegationsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DelegationsRevokeError::decode(value)
        }
    }
    pub type DeploymentsApplyInput = crate::__types::trellis::AuthDeploymentApplyRequest;
    pub type DeploymentsApplyOutput = crate::__types::trellis::AuthDeploymentMutationResponse;
    pub struct DeploymentsApply;
    impl DeploymentsApply {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Apply";
        pub const KEY: &'static str = "auth.Deployments.Apply";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Apply";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsApply"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsApplyError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsApplyError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsApply {
        type Input = DeploymentsApplyInput;
        type Output = DeploymentsApplyOutput;
        type Error = DeploymentsApplyError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsApplyError::decode(value)
        }
    }
    pub type DeploymentsCreateInput = crate::__types::trellis::AuthDeploymentCreateRequest;
    pub type DeploymentsCreateOutput = crate::__types::trellis::AuthDeploymentMutationResponse;
    pub struct DeploymentsCreate;
    impl DeploymentsCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Create";
        pub const KEY: &'static str = "auth.Deployments.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsCreate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsCreateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsCreateError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsCreate {
        type Input = DeploymentsCreateInput;
        type Output = DeploymentsCreateOutput;
        type Error = DeploymentsCreateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsCreateError::decode(value)
        }
    }
    pub type DeploymentsDisableInput = crate::__types::trellis::AuthDeploymentMutationRequest;
    pub type DeploymentsDisableOutput = crate::__types::trellis::AuthDeploymentMutationResponse;
    pub struct DeploymentsDisable;
    impl DeploymentsDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Disable";
        pub const KEY: &'static str = "auth.Deployments.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsDisable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsDisableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsDisableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsDisable {
        type Input = DeploymentsDisableInput;
        type Output = DeploymentsDisableOutput;
        type Error = DeploymentsDisableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsDisableError::decode(value)
        }
    }
    pub type DeploymentsEnableInput = crate::__types::trellis::AuthDeploymentMutationRequest;
    pub type DeploymentsEnableOutput = crate::__types::trellis::AuthDeploymentMutationResponse;
    pub struct DeploymentsEnable;
    impl DeploymentsEnable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Enable";
        pub const KEY: &'static str = "auth.Deployments.Enable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Enable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsEnable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsEnableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsEnableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsEnable {
        type Input = DeploymentsEnableInput;
        type Output = DeploymentsEnableOutput;
        type Error = DeploymentsEnableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsEnableError::decode(value)
        }
    }
    pub type DeploymentsGetInput = crate::__types::trellis::AuthDeploymentGetRequest;
    pub type DeploymentsGetOutput = crate::__types::trellis::AuthDeploymentGetResponse;
    pub struct DeploymentsGet;
    impl DeploymentsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Get";
        pub const KEY: &'static str = "auth.Deployments.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsGet {
        type Input = DeploymentsGetInput;
        type Output = DeploymentsGetOutput;
        type Error = DeploymentsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsGetError::decode(value)
        }
    }
    pub type DeploymentsListInput = crate::__types::trellis::AuthPageRequest;
    pub type DeploymentsListOutput = crate::__types::trellis::AuthDeploymentsListResponse;
    pub struct DeploymentsList;
    impl DeploymentsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.List";
        pub const KEY: &'static str = "auth.Deployments.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsList {
        type Input = DeploymentsListInput;
        type Output = DeploymentsListOutput;
        type Error = DeploymentsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsListError::decode(value)
        }
    }
    pub type DeploymentsRemoveInput = crate::__types::trellis::AuthDeploymentMutationRequest;
    pub type DeploymentsRemoveOutput = crate::__types::trellis::AuthDeploymentMutationResponse;
    pub struct DeploymentsRemove;
    impl DeploymentsRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Remove";
        pub const KEY: &'static str = "auth.Deployments.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deploymentsRemove"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeploymentsRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeploymentsRemoveError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeploymentsRemove {
        type Input = DeploymentsRemoveInput;
        type Output = DeploymentsRemoveOutput;
        type Error = DeploymentsRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeploymentsRemoveError::decode(value)
        }
    }
    pub type DevicesDisableInput = crate::__types::trellis::AuthInstanceMutationRequest;
    pub type DevicesDisableOutput = crate::__types::trellis::AuthInstanceMutationResponse;
    pub struct DevicesDisable;
    impl DevicesDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Disable";
        pub const KEY: &'static str = "auth.Devices.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devicesDisable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DevicesDisableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DevicesDisableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DevicesDisable {
        type Input = DevicesDisableInput;
        type Output = DevicesDisableOutput;
        type Error = DevicesDisableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DevicesDisableError::decode(value)
        }
    }
    pub type DevicesEnableInput = crate::__types::trellis::AuthInstanceMutationRequest;
    pub type DevicesEnableOutput = crate::__types::trellis::AuthInstanceMutationResponse;
    pub struct DevicesEnable;
    impl DevicesEnable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Enable";
        pub const KEY: &'static str = "auth.Devices.Enable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Enable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devicesEnable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DevicesEnableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DevicesEnableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DevicesEnable {
        type Input = DevicesEnableInput;
        type Output = DevicesEnableOutput;
        type Error = DevicesEnableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DevicesEnableError::decode(value)
        }
    }
    pub type DevicesListInput = crate::__types::trellis::AuthInstancesListRequest;
    pub type DevicesListOutput = crate::__types::trellis::AuthInstancesListResponse;
    pub struct DevicesList;
    impl DevicesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.List";
        pub const KEY: &'static str = "auth.Devices.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::devicesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DevicesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DevicesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DevicesList {
        type Input = DevicesListInput;
        type Output = DevicesListOutput;
        type Error = DevicesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DevicesListError::decode(value)
        }
    }
    pub type DevicesProvisionInput = crate::__types::trellis::AuthInstanceProvisionRequest;
    pub type DevicesProvisionOutput = crate::__types::trellis::AuthInstanceProvisionResponse;
    pub struct DevicesProvision;
    impl DevicesProvision {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Provision";
        pub const KEY: &'static str = "auth.Devices.Provision";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Provision";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devicesProvision"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DevicesProvisionError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DevicesProvisionError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DevicesProvision {
        type Input = DevicesProvisionInput;
        type Output = DevicesProvisionOutput;
        type Error = DevicesProvisionError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DevicesProvisionError::decode(value)
        }
    }
    pub type DevicesRemoveInput = crate::__types::trellis::AuthInstanceMutationRequest;
    pub type DevicesRemoveOutput = crate::__types::trellis::AuthInstanceMutationResponse;
    pub struct DevicesRemove;
    impl DevicesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Remove";
        pub const KEY: &'static str = "auth.Devices.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devicesRemove"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DevicesRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DevicesRemoveError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DevicesRemove {
        type Input = DevicesRemoveInput;
        type Output = DevicesRemoveOutput;
        type Error = DevicesRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DevicesRemoveError::decode(value)
        }
    }
    pub type IssuersResolveChainInput = crate::__types::trellis::AuthIssuerChainRequest;
    pub type IssuersResolveChainOutput = crate::__types::trellis::AuthIssuerChainResponse;
    pub struct IssuersResolveChain;
    impl IssuersResolveChain {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Issuers.ResolveChain";
        pub const KEY: &'static str = "auth.Issuers.ResolveChain";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Issuers.ResolveChain";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum IssuersResolveChainError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl IssuersResolveChainError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for IssuersResolveChain {
        type Input = IssuersResolveChainInput;
        type Output = IssuersResolveChainOutput;
        type Error = IssuersResolveChainError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            IssuersResolveChainError::decode(value)
        }
    }
    pub type IssuersRevokeInput = crate::__types::trellis::AuthIssuerRevokeRequest;
    pub type IssuersRevokeOutput = crate::__types::trellis::AuthIssuerRevokeResponse;
    pub struct IssuersRevoke;
    impl IssuersRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Issuers.Revoke";
        pub const KEY: &'static str = "auth.Issuers.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Issuers.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::issuersRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum IssuersRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl IssuersRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for IssuersRevoke {
        type Input = IssuersRevokeInput;
        type Output = IssuersRevokeOutput;
        type Error = IssuersRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            IssuersRevokeError::decode(value)
        }
    }
    pub type IssuersRotateInput = crate::__types::trellis::AuthIssuerRotateRequest;
    pub type IssuersRotateOutput = crate::__types::trellis::AuthIssuerRotateResponse;
    pub struct IssuersRotate;
    impl IssuersRotate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Issuers.Rotate";
        pub const KEY: &'static str = "auth.Issuers.Rotate";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Issuers.Rotate";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::issuersRotate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum IssuersRotateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl IssuersRotateError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for IssuersRotate {
        type Input = IssuersRotateInput;
        type Output = IssuersRotateOutput;
        type Error = IssuersRotateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            IssuersRotateError::decode(value)
        }
    }
    pub type OAuthClientsDeleteInput = crate::__types::trellis::AuthOAuthClientMutationRequest;
    pub type OAuthClientsDeleteOutput = crate::__types::trellis::AuthMutationResult;
    pub struct OAuthClientsDelete;
    impl OAuthClientsDelete {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OAuthClients.Delete";
        pub const KEY: &'static str = "auth.OAuthClients.Delete";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OAuthClients.Delete";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oauthClientsDelete"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OAuthClientsDeleteError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OAuthClientsDeleteError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OAuthClientsDelete {
        type Input = OAuthClientsDeleteInput;
        type Output = OAuthClientsDeleteOutput;
        type Error = OAuthClientsDeleteError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OAuthClientsDeleteError::decode(value)
        }
    }
    pub type OAuthClientsDisableInput = crate::__types::trellis::AuthOAuthClientMutationRequest;
    pub type OAuthClientsDisableOutput = crate::__types::trellis::AuthMutationResult;
    pub struct OAuthClientsDisable;
    impl OAuthClientsDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OAuthClients.Disable";
        pub const KEY: &'static str = "auth.OAuthClients.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OAuthClients.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oauthClientsDisable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OAuthClientsDisableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OAuthClientsDisableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OAuthClientsDisable {
        type Input = OAuthClientsDisableInput;
        type Output = OAuthClientsDisableOutput;
        type Error = OAuthClientsDisableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OAuthClientsDisableError::decode(value)
        }
    }
    pub type OAuthClientsGetInput = crate::__types::trellis::AuthOAuthClientGetRequest;
    pub type OAuthClientsGetOutput = crate::__types::trellis::AuthOAuthClientGetResponse;
    pub struct OAuthClientsGet;
    impl OAuthClientsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OAuthClients.Get";
        pub const KEY: &'static str = "auth.OAuthClients.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OAuthClients.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oauthClientsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OAuthClientsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OAuthClientsGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OAuthClientsGet {
        type Input = OAuthClientsGetInput;
        type Output = OAuthClientsGetOutput;
        type Error = OAuthClientsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OAuthClientsGetError::decode(value)
        }
    }
    pub type OAuthClientsListInput = crate::__types::trellis::AuthPageRequest;
    pub type OAuthClientsListOutput = crate::__types::trellis::AuthOAuthClientsListResponse;
    pub struct OAuthClientsList;
    impl OAuthClientsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OAuthClients.List";
        pub const KEY: &'static str = "auth.OAuthClients.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OAuthClients.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oauthClientsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OAuthClientsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OAuthClientsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OAuthClientsList {
        type Input = OAuthClientsListInput;
        type Output = OAuthClientsListOutput;
        type Error = OAuthClientsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OAuthClientsListError::decode(value)
        }
    }
    pub type OAuthClientsPutInput = crate::__types::trellis::AuthOAuthClientPutRequest;
    pub type OAuthClientsPutOutput = crate::__types::trellis::AuthOAuthClientGetResponse;
    pub struct OAuthClientsPut;
    impl OAuthClientsPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OAuthClients.Put";
        pub const KEY: &'static str = "auth.OAuthClients.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OAuthClients.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oauthClientsPut"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OAuthClientsPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OAuthClientsPutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OAuthClientsPut {
        type Input = OAuthClientsPutInput;
        type Output = OAuthClientsPutOutput;
        type Error = OAuthClientsPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OAuthClientsPutError::decode(value)
        }
    }
    pub type OIDCProvidersDeleteInput = crate::__types::trellis::AuthOIDCProviderMutationRequest;
    pub type OIDCProvidersDeleteOutput = crate::__types::trellis::AuthMutationResult;
    pub struct OIDCProvidersDelete;
    impl OIDCProvidersDelete {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCProviders.Delete";
        pub const KEY: &'static str = "auth.OIDCProviders.Delete";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCProviders.Delete";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcProvidersDelete"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCProvidersDeleteError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCProvidersDeleteError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCProvidersDelete {
        type Input = OIDCProvidersDeleteInput;
        type Output = OIDCProvidersDeleteOutput;
        type Error = OIDCProvidersDeleteError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCProvidersDeleteError::decode(value)
        }
    }
    pub type OIDCProvidersDisableInput = crate::__types::trellis::AuthOIDCProviderMutationRequest;
    pub type OIDCProvidersDisableOutput = crate::__types::trellis::AuthMutationResult;
    pub struct OIDCProvidersDisable;
    impl OIDCProvidersDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCProviders.Disable";
        pub const KEY: &'static str = "auth.OIDCProviders.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCProviders.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcProvidersDisable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCProvidersDisableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCProvidersDisableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCProvidersDisable {
        type Input = OIDCProvidersDisableInput;
        type Output = OIDCProvidersDisableOutput;
        type Error = OIDCProvidersDisableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCProvidersDisableError::decode(value)
        }
    }
    pub type OIDCProvidersGetInput = crate::__types::trellis::AuthOIDCProviderGetRequest;
    pub type OIDCProvidersGetOutput = crate::__types::trellis::AuthOIDCProviderGetResponse;
    pub struct OIDCProvidersGet;
    impl OIDCProvidersGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCProviders.Get";
        pub const KEY: &'static str = "auth.OIDCProviders.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCProviders.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcProvidersRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCProvidersGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCProvidersGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCProvidersGet {
        type Input = OIDCProvidersGetInput;
        type Output = OIDCProvidersGetOutput;
        type Error = OIDCProvidersGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCProvidersGetError::decode(value)
        }
    }
    pub type OIDCProvidersListInput = crate::__types::trellis::AuthPageRequest;
    pub type OIDCProvidersListOutput = crate::__types::trellis::AuthOIDCProvidersListResponse;
    pub struct OIDCProvidersList;
    impl OIDCProvidersList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCProviders.List";
        pub const KEY: &'static str = "auth.OIDCProviders.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCProviders.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcProvidersRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCProvidersListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCProvidersListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCProvidersList {
        type Input = OIDCProvidersListInput;
        type Output = OIDCProvidersListOutput;
        type Error = OIDCProvidersListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCProvidersListError::decode(value)
        }
    }
    pub type OIDCProvidersPutInput = crate::__types::trellis::AuthOIDCProviderPutRequest;
    pub type OIDCProvidersPutOutput = crate::__types::trellis::AuthOIDCProviderGetResponse;
    pub struct OIDCProvidersPut;
    impl OIDCProvidersPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCProviders.Put";
        pub const KEY: &'static str = "auth.OIDCProviders.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCProviders.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcProvidersPut"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCProvidersPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCProvidersPutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCProvidersPut {
        type Input = OIDCProvidersPutInput;
        type Output = OIDCProvidersPutOutput;
        type Error = OIDCProvidersPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCProvidersPutError::decode(value)
        }
    }
    pub type OIDCRoleMappingsDeleteInput =
        crate::__types::trellis::AuthOIDCRoleMappingDeleteRequest;
    pub type OIDCRoleMappingsDeleteOutput = crate::__types::trellis::AuthMutationResult;
    pub struct OIDCRoleMappingsDelete;
    impl OIDCRoleMappingsDelete {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCRoleMappings.Delete";
        pub const KEY: &'static str = "auth.OIDCRoleMappings.Delete";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCRoleMappings.Delete";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcRoleMappingsDelete"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCRoleMappingsDeleteError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCRoleMappingsDeleteError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCRoleMappingsDelete {
        type Input = OIDCRoleMappingsDeleteInput;
        type Output = OIDCRoleMappingsDeleteOutput;
        type Error = OIDCRoleMappingsDeleteError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCRoleMappingsDeleteError::decode(value)
        }
    }
    pub type OIDCRoleMappingsListInput = crate::__types::trellis::AuthOIDCRoleMappingsListRequest;
    pub type OIDCRoleMappingsListOutput = crate::__types::trellis::AuthOIDCRoleMappingsListResponse;
    pub struct OIDCRoleMappingsList;
    impl OIDCRoleMappingsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCRoleMappings.List";
        pub const KEY: &'static str = "auth.OIDCRoleMappings.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCRoleMappings.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcRoleMappingsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCRoleMappingsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCRoleMappingsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCRoleMappingsList {
        type Input = OIDCRoleMappingsListInput;
        type Output = OIDCRoleMappingsListOutput;
        type Error = OIDCRoleMappingsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCRoleMappingsListError::decode(value)
        }
    }
    pub type OIDCRoleMappingsPutInput = crate::__types::trellis::AuthOIDCRoleMappingPutRequest;
    pub type OIDCRoleMappingsPutOutput = crate::__types::trellis::AuthOIDCRoleMappingPutResponse;
    pub struct OIDCRoleMappingsPut;
    impl OIDCRoleMappingsPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.OIDCRoleMappings.Put";
        pub const KEY: &'static str = "auth.OIDCRoleMappings.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.OIDCRoleMappings.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::oidcRoleMappingsPut"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum OIDCRoleMappingsPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl OIDCRoleMappingsPutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for OIDCRoleMappingsPut {
        type Input = OIDCRoleMappingsPutInput;
        type Output = OIDCRoleMappingsPutOutput;
        type Error = OIDCRoleMappingsPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            OIDCRoleMappingsPutError::decode(value)
        }
    }
    pub type PlatformDelegationsListInput = crate::__types::trellis::AuthPageRequest;
    pub type PlatformDelegationsListOutput =
        crate::__types::trellis::AuthPlatformDelegationsListResponse;
    pub struct PlatformDelegationsList;
    impl PlatformDelegationsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PlatformDelegations.List";
        pub const KEY: &'static str = "auth.PlatformDelegations.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.PlatformDelegations.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::platformDelegationsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PlatformDelegationsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PlatformDelegationsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PlatformDelegationsList {
        type Input = PlatformDelegationsListInput;
        type Output = PlatformDelegationsListOutput;
        type Error = PlatformDelegationsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PlatformDelegationsListError::decode(value)
        }
    }
    pub type PlatformDelegationsPutInput =
        crate::__types::trellis::AuthPlatformDelegationPutRequest;
    pub type PlatformDelegationsPutOutput =
        crate::__types::trellis::AuthPlatformDelegationPutResponse;
    pub struct PlatformDelegationsPut;
    impl PlatformDelegationsPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PlatformDelegations.Put";
        pub const KEY: &'static str = "auth.PlatformDelegations.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.PlatformDelegations.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::platformDelegationsPut"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PlatformDelegationsPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PlatformDelegationsPutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PlatformDelegationsPut {
        type Input = PlatformDelegationsPutInput;
        type Output = PlatformDelegationsPutOutput;
        type Error = PlatformDelegationsPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PlatformDelegationsPutError::decode(value)
        }
    }
    pub type PlatformDelegationsRevokeInput =
        crate::__types::trellis::AuthPlatformDelegationRevokeRequest;
    pub type PlatformDelegationsRevokeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct PlatformDelegationsRevoke;
    impl PlatformDelegationsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PlatformDelegations.Revoke";
        pub const KEY: &'static str = "auth.PlatformDelegations.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.PlatformDelegations.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::platformDelegationsRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PlatformDelegationsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PlatformDelegationsRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PlatformDelegationsRevoke {
        type Input = PlatformDelegationsRevokeInput;
        type Output = PlatformDelegationsRevokeOutput;
        type Error = PlatformDelegationsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PlatformDelegationsRevokeError::decode(value)
        }
    }
    pub type PlatformPrivilegesAssignInput = crate::__types::trellis::AuthPlatformAssignRequest;
    pub type PlatformPrivilegesAssignOutput = crate::__types::trellis::AuthPlatformAssignResponse;
    pub struct PlatformPrivilegesAssign;
    impl PlatformPrivilegesAssign {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PlatformPrivileges.Assign";
        pub const KEY: &'static str = "auth.PlatformPrivileges.Assign";
        pub const SUBJECT: &'static str = "rpc.v1.auth.PlatformPrivileges.Assign";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::platformPrivilegesAssign"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PlatformPrivilegesAssignError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PlatformPrivilegesAssignError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PlatformPrivilegesAssign {
        type Input = PlatformPrivilegesAssignInput;
        type Output = PlatformPrivilegesAssignOutput;
        type Error = PlatformPrivilegesAssignError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PlatformPrivilegesAssignError::decode(value)
        }
    }
    pub type PlatformPrivilegesListInput = crate::__types::trellis::AuthPageRequest;
    pub type PlatformPrivilegesListOutput =
        crate::__types::trellis::AuthPlatformAssignmentsListResponse;
    pub struct PlatformPrivilegesList;
    impl PlatformPrivilegesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PlatformPrivileges.List";
        pub const KEY: &'static str = "auth.PlatformPrivileges.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.PlatformPrivileges.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::platformPrivilegesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PlatformPrivilegesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PlatformPrivilegesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PlatformPrivilegesList {
        type Input = PlatformPrivilegesListInput;
        type Output = PlatformPrivilegesListOutput;
        type Error = PlatformPrivilegesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PlatformPrivilegesListError::decode(value)
        }
    }
    pub type PlatformPrivilegesRevokeInput = crate::__types::trellis::AuthPlatformRevokeRequest;
    pub type PlatformPrivilegesRevokeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct PlatformPrivilegesRevoke;
    impl PlatformPrivilegesRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.PlatformPrivileges.Revoke";
        pub const KEY: &'static str = "auth.PlatformPrivileges.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.PlatformPrivileges.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::platformPrivilegesRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PlatformPrivilegesRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PlatformPrivilegesRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PlatformPrivilegesRevoke {
        type Input = PlatformPrivilegesRevokeInput;
        type Output = PlatformPrivilegesRevokeOutput;
        type Error = PlatformPrivilegesRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PlatformPrivilegesRevokeError::decode(value)
        }
    }
    pub type PrincipalsEffectiveAccessInput = crate::__types::trellis::AuthPrincipalRequest;
    pub type PrincipalsEffectiveAccessOutput = crate::__types::trellis::AuthEffectiveAccessResponse;
    pub struct PrincipalsEffectiveAccess;
    impl PrincipalsEffectiveAccess {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Principals.EffectiveAccess";
        pub const KEY: &'static str = "auth.Principals.EffectiveAccess";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Principals.EffectiveAccess";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::principalsEffectiveAccess"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PrincipalsEffectiveAccessError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PrincipalsEffectiveAccessError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for PrincipalsEffectiveAccess {
        type Input = PrincipalsEffectiveAccessInput;
        type Output = PrincipalsEffectiveAccessOutput;
        type Error = PrincipalsEffectiveAccessError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PrincipalsEffectiveAccessError::decode(value)
        }
    }
    pub type ResourcesGetInput = crate::__types::trellis::AuthResourceGetRequest;
    pub type ResourcesGetOutput = crate::__types::trellis::AuthResourceGetResponse;
    pub struct ResourcesGet;
    impl ResourcesGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.Get";
        pub const KEY: &'static str = "auth.Resources.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Resources.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::resourcesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ResourcesGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ResourcesGet {
        type Input = ResourcesGetInput;
        type Output = ResourcesGetOutput;
        type Error = ResourcesGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ResourcesGetError::decode(value)
        }
    }
    pub type ResourcesListInput = crate::__types::trellis::AuthResourcesListRequest;
    pub type ResourcesListOutput = crate::__types::trellis::AuthResourcesListResponse;
    pub struct ResourcesList;
    impl ResourcesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.List";
        pub const KEY: &'static str = "auth.Resources.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Resources.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::resourcesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ResourcesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ResourcesList {
        type Input = ResourcesListInput;
        type Output = ResourcesListOutput;
        type Error = ResourcesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ResourcesListError::decode(value)
        }
    }
    pub type ResourcesRemoveInput = crate::__types::trellis::AuthResourceRemoveRequest;
    pub type ResourcesRemoveOutput = crate::__types::trellis::AuthMutationResult;
    pub struct ResourcesRemove;
    impl ResourcesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Resources.Remove";
        pub const KEY: &'static str = "auth.Resources.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Resources.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::resourcesRemove"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ResourcesRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ResourcesRemoveError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ResourcesRemove {
        type Input = ResourcesRemoveInput;
        type Output = ResourcesRemoveOutput;
        type Error = ResourcesRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ResourcesRemoveError::decode(value)
        }
    }
    pub type RoleAssignmentsAssignInput = crate::__types::trellis::AuthRoleAssignRequest;
    pub type RoleAssignmentsAssignOutput = crate::__types::trellis::AuthRoleAssignResponse;
    pub struct RoleAssignmentsAssign;
    impl RoleAssignmentsAssign {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.RoleAssignments.Assign";
        pub const KEY: &'static str = "auth.RoleAssignments.Assign";
        pub const SUBJECT: &'static str = "rpc.v1.auth.RoleAssignments.Assign";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::roleAssignmentsAssign"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RoleAssignmentsAssignError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RoleAssignmentsAssignError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RoleAssignmentsAssign {
        type Input = RoleAssignmentsAssignInput;
        type Output = RoleAssignmentsAssignOutput;
        type Error = RoleAssignmentsAssignError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RoleAssignmentsAssignError::decode(value)
        }
    }
    pub type RoleAssignmentsListInput = crate::__types::trellis::AuthRoleAssignmentsListRequest;
    pub type RoleAssignmentsListOutput = crate::__types::trellis::AuthRoleAssignmentsListResponse;
    pub struct RoleAssignmentsList;
    impl RoleAssignmentsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.RoleAssignments.List";
        pub const KEY: &'static str = "auth.RoleAssignments.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.RoleAssignments.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::roleAssignmentsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RoleAssignmentsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RoleAssignmentsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RoleAssignmentsList {
        type Input = RoleAssignmentsListInput;
        type Output = RoleAssignmentsListOutput;
        type Error = RoleAssignmentsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RoleAssignmentsListError::decode(value)
        }
    }
    pub type RoleAssignmentsRevokeInput = crate::__types::trellis::AuthRoleRevokeRequest;
    pub type RoleAssignmentsRevokeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct RoleAssignmentsRevoke;
    impl RoleAssignmentsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.RoleAssignments.Revoke";
        pub const KEY: &'static str = "auth.RoleAssignments.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.RoleAssignments.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::roleAssignmentsRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RoleAssignmentsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RoleAssignmentsRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RoleAssignmentsRevoke {
        type Input = RoleAssignmentsRevokeInput;
        type Output = RoleAssignmentsRevokeOutput;
        type Error = RoleAssignmentsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RoleAssignmentsRevokeError::decode(value)
        }
    }
    pub type RolesDeleteInput = crate::__types::trellis::AuthRoleDeleteRequest;
    pub type RolesDeleteOutput = crate::__types::trellis::AuthMutationResult;
    pub struct RolesDelete;
    impl RolesDelete {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Roles.Delete";
        pub const KEY: &'static str = "auth.Roles.Delete";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Roles.Delete";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::rolesDelete"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RolesDeleteError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RolesDeleteError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RolesDelete {
        type Input = RolesDeleteInput;
        type Output = RolesDeleteOutput;
        type Error = RolesDeleteError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RolesDeleteError::decode(value)
        }
    }
    pub type RolesGetInput = crate::__types::trellis::AuthRoleGetRequest;
    pub type RolesGetOutput = crate::__types::trellis::AuthRoleGetResponse;
    pub struct RolesGet;
    impl RolesGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Roles.Get";
        pub const KEY: &'static str = "auth.Roles.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Roles.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::rolesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RolesGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RolesGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RolesGet {
        type Input = RolesGetInput;
        type Output = RolesGetOutput;
        type Error = RolesGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RolesGetError::decode(value)
        }
    }
    pub type RolesListInput = crate::__types::trellis::AuthPageRequest;
    pub type RolesListOutput = crate::__types::trellis::AuthRolesListResponse;
    pub struct RolesList;
    impl RolesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Roles.List";
        pub const KEY: &'static str = "auth.Roles.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Roles.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::rolesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RolesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RolesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RolesList {
        type Input = RolesListInput;
        type Output = RolesListOutput;
        type Error = RolesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RolesListError::decode(value)
        }
    }
    pub type RolesPutInput = crate::__types::trellis::AuthRolePutRequest;
    pub type RolesPutOutput = crate::__types::trellis::AuthRolePutResponse;
    pub struct RolesPut;
    impl RolesPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Roles.Put";
        pub const KEY: &'static str = "auth.Roles.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Roles.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::rolesPut"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RolesPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RolesPutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for RolesPut {
        type Input = RolesPutInput;
        type Output = RolesPutOutput;
        type Error = RolesPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RolesPutError::decode(value)
        }
    }
    pub type ServiceInstancesDisableInput = crate::__types::trellis::AuthInstanceMutationRequest;
    pub type ServiceInstancesDisableOutput = crate::__types::trellis::AuthInstanceMutationResponse;
    pub struct ServiceInstancesDisable;
    impl ServiceInstancesDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Disable";
        pub const KEY: &'static str = "auth.ServiceInstances.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::serviceInstancesDisable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ServiceInstancesDisableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ServiceInstancesDisableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ServiceInstancesDisable {
        type Input = ServiceInstancesDisableInput;
        type Output = ServiceInstancesDisableOutput;
        type Error = ServiceInstancesDisableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ServiceInstancesDisableError::decode(value)
        }
    }
    pub type ServiceInstancesEnableInput = crate::__types::trellis::AuthInstanceMutationRequest;
    pub type ServiceInstancesEnableOutput = crate::__types::trellis::AuthInstanceMutationResponse;
    pub struct ServiceInstancesEnable;
    impl ServiceInstancesEnable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Enable";
        pub const KEY: &'static str = "auth.ServiceInstances.Enable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Enable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::serviceInstancesEnable"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ServiceInstancesEnableError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ServiceInstancesEnableError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ServiceInstancesEnable {
        type Input = ServiceInstancesEnableInput;
        type Output = ServiceInstancesEnableOutput;
        type Error = ServiceInstancesEnableError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ServiceInstancesEnableError::decode(value)
        }
    }
    pub type ServiceInstancesListInput = crate::__types::trellis::AuthInstancesListRequest;
    pub type ServiceInstancesListOutput = crate::__types::trellis::AuthInstancesListResponse;
    pub struct ServiceInstancesList;
    impl ServiceInstancesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.List";
        pub const KEY: &'static str = "auth.ServiceInstances.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::serviceInstancesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ServiceInstancesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ServiceInstancesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ServiceInstancesList {
        type Input = ServiceInstancesListInput;
        type Output = ServiceInstancesListOutput;
        type Error = ServiceInstancesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ServiceInstancesListError::decode(value)
        }
    }
    pub type ServiceInstancesProvisionInput = crate::__types::trellis::AuthInstanceProvisionRequest;
    pub type ServiceInstancesProvisionOutput =
        crate::__types::trellis::AuthInstanceProvisionResponse;
    pub struct ServiceInstancesProvision;
    impl ServiceInstancesProvision {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Provision";
        pub const KEY: &'static str = "auth.ServiceInstances.Provision";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Provision";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::serviceInstancesProvision"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ServiceInstancesProvisionError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ServiceInstancesProvisionError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ServiceInstancesProvision {
        type Input = ServiceInstancesProvisionInput;
        type Output = ServiceInstancesProvisionOutput;
        type Error = ServiceInstancesProvisionError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ServiceInstancesProvisionError::decode(value)
        }
    }
    pub type ServiceInstancesRemoveInput = crate::__types::trellis::AuthInstanceMutationRequest;
    pub type ServiceInstancesRemoveOutput = crate::__types::trellis::AuthInstanceMutationResponse;
    pub struct ServiceInstancesRemove;
    impl ServiceInstancesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Remove";
        pub const KEY: &'static str = "auth.ServiceInstances.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::serviceInstancesRemove"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ServiceInstancesRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ServiceInstancesRemoveError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ServiceInstancesRemove {
        type Input = ServiceInstancesRemoveInput;
        type Output = ServiceInstancesRemoveOutput;
        type Error = ServiceInstancesRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ServiceInstancesRemoveError::decode(value)
        }
    }
    pub type SessionsListInput = crate::__types::trellis::AuthPageRequest;
    pub type SessionsListOutput = crate::__types::trellis::AuthSessionsListResponse;
    pub struct SessionsList;
    impl SessionsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.List";
        pub const KEY: &'static str = "auth.Sessions.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::sessionsRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SessionsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SessionsListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for SessionsList {
        type Input = SessionsListInput;
        type Output = SessionsListOutput;
        type Error = SessionsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SessionsListError::decode(value)
        }
    }
    pub type SessionsLogoutInput = crate::__types::trellis::AuthEmpty;
    pub type SessionsLogoutOutput = crate::__types::trellis::AuthSessionsLogoutResponse;
    pub struct SessionsLogout;
    impl SessionsLogout {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.Logout";
        pub const KEY: &'static str = "auth.Sessions.Logout";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.Logout";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SessionsLogoutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SessionsLogoutError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for SessionsLogout {
        type Input = SessionsLogoutInput;
        type Output = SessionsLogoutOutput;
        type Error = SessionsLogoutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SessionsLogoutError::decode(value)
        }
    }
    pub type SessionsMeInput = crate::__types::trellis::AuthEmpty;
    pub type SessionsMeOutput = crate::__types::trellis::AuthSessionsMeResponse;
    pub struct SessionsMe;
    impl SessionsMe {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.Me";
        pub const KEY: &'static str = "auth.Sessions.Me";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.Me";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SessionsMeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SessionsMeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for SessionsMe {
        type Input = SessionsMeInput;
        type Output = SessionsMeOutput;
        type Error = SessionsMeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SessionsMeError::decode(value)
        }
    }
    pub type SessionsRevokeInput = crate::__types::trellis::AuthLoginRevokeRequest;
    pub type SessionsRevokeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct SessionsRevoke;
    impl SessionsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.Revoke";
        pub const KEY: &'static str = "auth.Sessions.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::sessionsRevoke"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SessionsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SessionsRevokeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for SessionsRevoke {
        type Input = SessionsRevokeInput;
        type Output = SessionsRevokeOutput;
        type Error = SessionsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SessionsRevokeError::decode(value)
        }
    }
    pub type UserIdentitiesListInput = crate::__types::trellis::AuthPageRequest;
    pub type UserIdentitiesListOutput = crate::__types::trellis::AuthUserIdentitiesListResponse;
    pub struct UserIdentitiesList;
    impl UserIdentitiesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.UserIdentities.List";
        pub const KEY: &'static str = "auth.UserIdentities.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.UserIdentities.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::identitiesRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UserIdentitiesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UserIdentitiesListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UserIdentitiesList {
        type Input = UserIdentitiesListInput;
        type Output = UserIdentitiesListOutput;
        type Error = UserIdentitiesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UserIdentitiesListError::decode(value)
        }
    }
    pub type UserIdentitiesUnlinkInput = crate::__types::trellis::AuthUserIdentityUnlinkRequest;
    pub type UserIdentitiesUnlinkOutput = crate::__types::trellis::AuthMutationResult;
    pub struct UserIdentitiesUnlink;
    impl UserIdentitiesUnlink {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.UserIdentities.Unlink";
        pub const KEY: &'static str = "auth.UserIdentities.Unlink";
        pub const SUBJECT: &'static str = "rpc.v1.auth.UserIdentities.Unlink";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::identitiesManage"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UserIdentitiesUnlinkError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UserIdentitiesUnlinkError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UserIdentitiesUnlink {
        type Input = UserIdentitiesUnlinkInput;
        type Output = UserIdentitiesUnlinkOutput;
        type Error = UserIdentitiesUnlinkError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UserIdentitiesUnlinkError::decode(value)
        }
    }
    pub type UsersCreateInput = crate::__types::trellis::AuthUserCreateRequest;
    pub type UsersCreateOutput = crate::__types::trellis::AuthUserGetResponse;
    pub struct UsersCreate;
    impl UsersCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Create";
        pub const KEY: &'static str = "auth.Users.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::usersCreate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersCreateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersCreateError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersCreate {
        type Input = UsersCreateInput;
        type Output = UsersCreateOutput;
        type Error = UsersCreateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersCreateError::decode(value)
        }
    }
    pub type UsersGetInput = crate::__types::trellis::AuthUserGetRequest;
    pub type UsersGetOutput = crate::__types::trellis::AuthUserGetResponse;
    pub struct UsersGet;
    impl UsersGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Get";
        pub const KEY: &'static str = "auth.Users.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::usersRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersGetError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersGet {
        type Input = UsersGetInput;
        type Output = UsersGetOutput;
        type Error = UsersGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersGetError::decode(value)
        }
    }
    pub type UsersIdentityLinkCreateInput = crate::__types::trellis::AuthIdentityLinkCreateRequest;
    pub type UsersIdentityLinkCreateOutput =
        crate::__types::trellis::AuthIdentityLinkCreateResponse;
    pub struct UsersIdentityLinkCreate;
    impl UsersIdentityLinkCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.IdentityLink.Create";
        pub const KEY: &'static str = "auth.Users.IdentityLink.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.IdentityLink.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::identitiesManage"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersIdentityLinkCreateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersIdentityLinkCreateError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersIdentityLinkCreate {
        type Input = UsersIdentityLinkCreateInput;
        type Output = UsersIdentityLinkCreateOutput;
        type Error = UsersIdentityLinkCreateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersIdentityLinkCreateError::decode(value)
        }
    }
    pub type UsersListInput = crate::__types::trellis::AuthPageRequest;
    pub type UsersListOutput = crate::__types::trellis::AuthUsersListResponse;
    pub struct UsersList;
    impl UsersList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.List";
        pub const KEY: &'static str = "auth.Users.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::usersRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersListError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersList {
        type Input = UsersListInput;
        type Output = UsersListOutput;
        type Error = UsersListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersListError::decode(value)
        }
    }
    pub type UsersPasswordChangeInput = crate::__types::trellis::AuthPasswordChangeRequest;
    pub type UsersPasswordChangeOutput = crate::__types::trellis::AuthMutationResult;
    pub struct UsersPasswordChange;
    impl UsersPasswordChange {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Password.Change";
        pub const KEY: &'static str = "auth.Users.Password.Change";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Password.Change";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::passwordChange"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersPasswordChangeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersPasswordChangeError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersPasswordChange {
        type Input = UsersPasswordChangeInput;
        type Output = UsersPasswordChangeOutput;
        type Error = UsersPasswordChangeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersPasswordChangeError::decode(value)
        }
    }
    pub type UsersPasswordResetCreateInput =
        crate::__types::trellis::AuthPasswordResetCreateRequest;
    pub type UsersPasswordResetCreateOutput =
        crate::__types::trellis::AuthPasswordResetCreateResponse;
    pub struct UsersPasswordResetCreate;
    impl UsersPasswordResetCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.PasswordReset.Create";
        pub const KEY: &'static str = "auth.Users.PasswordReset.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.PasswordReset.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::usersPasswordReset"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersPasswordResetCreateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersPasswordResetCreateError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersPasswordResetCreate {
        type Input = UsersPasswordResetCreateInput;
        type Output = UsersPasswordResetCreateOutput;
        type Error = UsersPasswordResetCreateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersPasswordResetCreateError::decode(value)
        }
    }
    pub type UsersResolveInput = crate::__types::trellis::AuthEmpty;
    pub type UsersResolveOutput = crate::__types::trellis::AuthUserGetResponse;
    pub struct UsersResolve;
    impl UsersResolve {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Resolve";
        pub const KEY: &'static str = "auth.Users.Resolve";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Resolve";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::accountRead"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersResolveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersResolveError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersResolve {
        type Input = UsersResolveInput;
        type Output = UsersResolveOutput;
        type Error = UsersResolveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersResolveError::decode(value)
        }
    }
    pub type UsersUpdateInput = crate::__types::trellis::AuthUserUpdateRequest;
    pub type UsersUpdateOutput = crate::__types::trellis::AuthUserGetResponse;
    pub struct UsersUpdate;
    impl UsersUpdate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Update";
        pub const KEY: &'static str = "auth.Users.Update";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Update";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::usersUpdate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum UsersUpdateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl UsersUpdateError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.auth@v1::AuthError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::AuthError>(value)
                        .map(|value| value.map(Self::AuthError))
                }
                Some("trellis.auth@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.auth@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for UsersUpdate {
        type Input = UsersUpdateInput;
        type Output = UsersUpdateOutput;
        type Error = UsersUpdateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            UsersUpdateError::decode(value)
        }
    }
}
pub mod operations {}
pub mod events {
    pub type AuthorizationSessionsRetiredEvent = crate::__types::trellis::AuthSecurityEvent;
    pub struct AuthorizationSessionsRetired;
    impl AuthorizationSessionsRetired {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.AuthorizationSessions.Retired";
        pub const KEY: &'static str = "auth.AuthorizationSessions.Retired";
        pub const SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.AuthorizationSessions.Retired";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.AuthorizationSessions.Retired";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        pub const DELEGATED_PUBLISH: bool = false;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::securityObserve"];
    }
    impl trellis_rs::generated::EventDescriptor for AuthorizationSessionsRetired {
        type Event = AuthorizationSessionsRetiredEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type ConnectionsClosedEvent = crate::__types::trellis::AuthSecurityEvent;
    pub struct ConnectionsClosed;
    impl ConnectionsClosed {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Connections.Closed";
        pub const KEY: &'static str = "auth.Connections.Closed";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Closed";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Closed";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        pub const DELEGATED_PUBLISH: bool = false;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::securityObserve"];
    }
    impl trellis_rs::generated::EventDescriptor for ConnectionsClosed {
        type Event = ConnectionsClosedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type ConnectionsKickedEvent = crate::__types::trellis::AuthSecurityEvent;
    pub struct ConnectionsKicked;
    impl ConnectionsKicked {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Connections.Kicked";
        pub const KEY: &'static str = "auth.Connections.Kicked";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Kicked";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Kicked";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        pub const DELEGATED_PUBLISH: bool = false;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::securityObserve"];
    }
    impl trellis_rs::generated::EventDescriptor for ConnectionsKicked {
        type Event = ConnectionsKickedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type ConnectionsOpenedEvent = crate::__types::trellis::AuthSecurityEvent;
    pub struct ConnectionsOpened;
    impl ConnectionsOpened {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Connections.Opened";
        pub const KEY: &'static str = "auth.Connections.Opened";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Opened";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Opened";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        pub const DELEGATED_PUBLISH: bool = false;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::securityObserve"];
    }
    impl trellis_rs::generated::EventDescriptor for ConnectionsOpened {
        type Event = ConnectionsOpenedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type IssuersRevokedEvent = crate::__types::trellis::AuthSecurityEvent;
    pub struct IssuersRevoked;
    impl IssuersRevoked {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Issuers.Revoked";
        pub const KEY: &'static str = "auth.Issuers.Revoked";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Issuers.Revoked";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Issuers.Revoked";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        pub const DELEGATED_PUBLISH: bool = false;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::securityObserve"];
    }
    impl trellis_rs::generated::EventDescriptor for IssuersRevoked {
        type Event = IssuersRevokedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type IssuersRotatedEvent = crate::__types::trellis::AuthSecurityEvent;
    pub struct IssuersRotated;
    impl IssuersRotated {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Issuers.Rotated";
        pub const KEY: &'static str = "auth.Issuers.Rotated";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Issuers.Rotated";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Issuers.Rotated";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        pub const DELEGATED_PUBLISH: bool = false;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::securityObserve"];
    }
    impl trellis_rs::generated::EventDescriptor for IssuersRotated {
        type Event = IssuersRotatedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
}
pub mod lives {}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::AdmissionRenew>();
    router.register_rpc_metadata::<rpc::ApisAccept>();
    router.register_rpc_metadata::<rpc::ApisForceReplace>();
    router.register_rpc_metadata::<rpc::ApisGet>();
    router.register_rpc_metadata::<rpc::ApisList>();
    router.register_rpc_metadata::<rpc::ApisReview>();
    router.register_rpc_metadata::<rpc::AuthorityRenew>();
    router.register_rpc_metadata::<rpc::AuthorityResolve>();
    router.register_rpc_metadata::<rpc::AuthorityStatus>();
    router.register_rpc_metadata::<rpc::AuthorizationSessionsList>();
    router.register_rpc_metadata::<rpc::AuthorizationSessionsRevoke>();
    router.register_rpc_metadata::<rpc::CapabilitiesList>();
    router.register_rpc_metadata::<rpc::CapabilityGrantsGrant>();
    router.register_rpc_metadata::<rpc::CapabilityGrantsList>();
    router.register_rpc_metadata::<rpc::CapabilityGrantsRevoke>();
    router.register_rpc_metadata::<rpc::ConnectionsKick>();
    router.register_rpc_metadata::<rpc::ConnectionsList>();
    router.register_rpc_metadata::<rpc::DelegationsList>();
    router.register_rpc_metadata::<rpc::DelegationsReview>();
    router.register_rpc_metadata::<rpc::DelegationsRevoke>();
    router.register_rpc_metadata::<rpc::DeploymentsApply>();
    router.register_rpc_metadata::<rpc::DeploymentsCreate>();
    router.register_rpc_metadata::<rpc::DeploymentsDisable>();
    router.register_rpc_metadata::<rpc::DeploymentsEnable>();
    router.register_rpc_metadata::<rpc::DeploymentsGet>();
    router.register_rpc_metadata::<rpc::DeploymentsList>();
    router.register_rpc_metadata::<rpc::DeploymentsRemove>();
    router.register_rpc_metadata::<rpc::DevicesDisable>();
    router.register_rpc_metadata::<rpc::DevicesEnable>();
    router.register_rpc_metadata::<rpc::DevicesList>();
    router.register_rpc_metadata::<rpc::DevicesProvision>();
    router.register_rpc_metadata::<rpc::DevicesRemove>();
    router.register_rpc_metadata::<rpc::IssuersResolveChain>();
    router.register_rpc_metadata::<rpc::IssuersRevoke>();
    router.register_rpc_metadata::<rpc::IssuersRotate>();
    router.register_rpc_metadata::<rpc::OAuthClientsDelete>();
    router.register_rpc_metadata::<rpc::OAuthClientsDisable>();
    router.register_rpc_metadata::<rpc::OAuthClientsGet>();
    router.register_rpc_metadata::<rpc::OAuthClientsList>();
    router.register_rpc_metadata::<rpc::OAuthClientsPut>();
    router.register_rpc_metadata::<rpc::OIDCProvidersDelete>();
    router.register_rpc_metadata::<rpc::OIDCProvidersDisable>();
    router.register_rpc_metadata::<rpc::OIDCProvidersGet>();
    router.register_rpc_metadata::<rpc::OIDCProvidersList>();
    router.register_rpc_metadata::<rpc::OIDCProvidersPut>();
    router.register_rpc_metadata::<rpc::OIDCRoleMappingsDelete>();
    router.register_rpc_metadata::<rpc::OIDCRoleMappingsList>();
    router.register_rpc_metadata::<rpc::OIDCRoleMappingsPut>();
    router.register_rpc_metadata::<rpc::PlatformDelegationsList>();
    router.register_rpc_metadata::<rpc::PlatformDelegationsPut>();
    router.register_rpc_metadata::<rpc::PlatformDelegationsRevoke>();
    router.register_rpc_metadata::<rpc::PlatformPrivilegesAssign>();
    router.register_rpc_metadata::<rpc::PlatformPrivilegesList>();
    router.register_rpc_metadata::<rpc::PlatformPrivilegesRevoke>();
    router.register_rpc_metadata::<rpc::PrincipalsEffectiveAccess>();
    router.register_rpc_metadata::<rpc::ResourcesGet>();
    router.register_rpc_metadata::<rpc::ResourcesList>();
    router.register_rpc_metadata::<rpc::ResourcesRemove>();
    router.register_rpc_metadata::<rpc::RoleAssignmentsAssign>();
    router.register_rpc_metadata::<rpc::RoleAssignmentsList>();
    router.register_rpc_metadata::<rpc::RoleAssignmentsRevoke>();
    router.register_rpc_metadata::<rpc::RolesDelete>();
    router.register_rpc_metadata::<rpc::RolesGet>();
    router.register_rpc_metadata::<rpc::RolesList>();
    router.register_rpc_metadata::<rpc::RolesPut>();
    router.register_rpc_metadata::<rpc::ServiceInstancesDisable>();
    router.register_rpc_metadata::<rpc::ServiceInstancesEnable>();
    router.register_rpc_metadata::<rpc::ServiceInstancesList>();
    router.register_rpc_metadata::<rpc::ServiceInstancesProvision>();
    router.register_rpc_metadata::<rpc::ServiceInstancesRemove>();
    router.register_rpc_metadata::<rpc::SessionsList>();
    router.register_rpc_metadata::<rpc::SessionsLogout>();
    router.register_rpc_metadata::<rpc::SessionsMe>();
    router.register_rpc_metadata::<rpc::SessionsRevoke>();
    router.register_rpc_metadata::<rpc::UserIdentitiesList>();
    router.register_rpc_metadata::<rpc::UserIdentitiesUnlink>();
    router.register_rpc_metadata::<rpc::UsersCreate>();
    router.register_rpc_metadata::<rpc::UsersGet>();
    router.register_rpc_metadata::<rpc::UsersIdentityLinkCreate>();
    router.register_rpc_metadata::<rpc::UsersList>();
    router.register_rpc_metadata::<rpc::UsersPasswordChange>();
    router.register_rpc_metadata::<rpc::UsersPasswordResetCreate>();
    router.register_rpc_metadata::<rpc::UsersResolve>();
    router.register_rpc_metadata::<rpc::UsersUpdate>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn admission_renew(
        &self,
        input: &rpc::AdmissionRenewInput,
    ) -> Result<rpc::AdmissionRenewOutput, trellis_rs::client::CallError<rpc::AdmissionRenewError>>
    {
        self.inner.call::<rpc::AdmissionRenew>(input).await
    }
    pub async fn apis_accept(
        &self,
        input: &rpc::ApisAcceptInput,
    ) -> Result<rpc::ApisAcceptOutput, trellis_rs::client::CallError<rpc::ApisAcceptError>> {
        self.inner.call::<rpc::ApisAccept>(input).await
    }
    pub async fn apis_force_replace(
        &self,
        input: &rpc::ApisForceReplaceInput,
    ) -> Result<
        rpc::ApisForceReplaceOutput,
        trellis_rs::client::CallError<rpc::ApisForceReplaceError>,
    > {
        self.inner.call::<rpc::ApisForceReplace>(input).await
    }
    pub async fn apis_get(
        &self,
        input: &rpc::ApisGetInput,
    ) -> Result<rpc::ApisGetOutput, trellis_rs::client::CallError<rpc::ApisGetError>> {
        self.inner.call::<rpc::ApisGet>(input).await
    }
    pub async fn apis_list(
        &self,
        input: &rpc::ApisListInput,
    ) -> Result<rpc::ApisListOutput, trellis_rs::client::CallError<rpc::ApisListError>> {
        self.inner.call::<rpc::ApisList>(input).await
    }
    pub fn apis_list_pages(
        &self,
        input: rpc::ApisListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ApisListOutput, crate::PaginationError<rpc::ApisListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .apis_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn apis_list_items(
        &self,
        input: rpc::ApisListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthAcceptedApi,
            crate::PaginationError<rpc::ApisListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthAcceptedApi>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .apis_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn apis_review(
        &self,
        input: &rpc::ApisReviewInput,
    ) -> Result<rpc::ApisReviewOutput, trellis_rs::client::CallError<rpc::ApisReviewError>> {
        self.inner.call::<rpc::ApisReview>(input).await
    }
    pub async fn authority_renew(
        &self,
        input: &rpc::AuthorityRenewInput,
    ) -> Result<rpc::AuthorityRenewOutput, trellis_rs::client::CallError<rpc::AuthorityRenewError>>
    {
        self.inner.call::<rpc::AuthorityRenew>(input).await
    }
    pub async fn authority_resolve(
        &self,
        input: &rpc::AuthorityResolveInput,
    ) -> Result<
        rpc::AuthorityResolveOutput,
        trellis_rs::client::CallError<rpc::AuthorityResolveError>,
    > {
        self.inner.call::<rpc::AuthorityResolve>(input).await
    }
    pub async fn authority_status(
        &self,
        input: &rpc::AuthorityStatusInput,
    ) -> Result<rpc::AuthorityStatusOutput, trellis_rs::client::CallError<rpc::AuthorityStatusError>>
    {
        self.inner.call::<rpc::AuthorityStatus>(input).await
    }
    pub async fn authorization_sessions_list(
        &self,
        input: &rpc::AuthorizationSessionsListInput,
    ) -> Result<
        rpc::AuthorizationSessionsListOutput,
        trellis_rs::client::CallError<rpc::AuthorizationSessionsListError>,
    > {
        self.inner
            .call::<rpc::AuthorizationSessionsList>(input)
            .await
    }
    pub fn authorization_sessions_list_pages(
        &self,
        input: rpc::AuthorizationSessionsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::AuthorizationSessionsListOutput,
            crate::PaginationError<rpc::AuthorizationSessionsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .authorization_sessions_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn authorization_sessions_list_items(
        &self,
        input: rpc::AuthorizationSessionsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthAuthorizationSession,
            crate::PaginationError<rpc::AuthorizationSessionsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthAuthorizationSession>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .authorization_sessions_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn authorization_sessions_revoke(
        &self,
        input: &rpc::AuthorizationSessionsRevokeInput,
    ) -> Result<
        rpc::AuthorizationSessionsRevokeOutput,
        trellis_rs::client::CallError<rpc::AuthorizationSessionsRevokeError>,
    > {
        self.inner
            .call::<rpc::AuthorizationSessionsRevoke>(input)
            .await
    }
    pub async fn capabilities_list(
        &self,
        input: &rpc::CapabilitiesListInput,
    ) -> Result<
        rpc::CapabilitiesListOutput,
        trellis_rs::client::CallError<rpc::CapabilitiesListError>,
    > {
        self.inner.call::<rpc::CapabilitiesList>(input).await
    }
    pub fn capabilities_list_pages(
        &self,
        input: rpc::CapabilitiesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::CapabilitiesListOutput, crate::PaginationError<rpc::CapabilitiesListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .capabilities_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn capabilities_list_items(
        &self,
        input: rpc::CapabilitiesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthCapabilityDefinition,
            crate::PaginationError<rpc::CapabilitiesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthCapabilityDefinition>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .capabilities_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn capability_grants_grant(
        &self,
        input: &rpc::CapabilityGrantsGrantInput,
    ) -> Result<
        rpc::CapabilityGrantsGrantOutput,
        trellis_rs::client::CallError<rpc::CapabilityGrantsGrantError>,
    > {
        self.inner.call::<rpc::CapabilityGrantsGrant>(input).await
    }
    pub async fn capability_grants_list(
        &self,
        input: &rpc::CapabilityGrantsListInput,
    ) -> Result<
        rpc::CapabilityGrantsListOutput,
        trellis_rs::client::CallError<rpc::CapabilityGrantsListError>,
    > {
        self.inner.call::<rpc::CapabilityGrantsList>(input).await
    }
    pub fn capability_grants_list_pages(
        &self,
        input: rpc::CapabilityGrantsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::CapabilityGrantsListOutput,
            crate::PaginationError<rpc::CapabilityGrantsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .capability_grants_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn capability_grants_list_items(
        &self,
        input: rpc::CapabilityGrantsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthCapabilityGrant,
            crate::PaginationError<rpc::CapabilityGrantsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthCapabilityGrant>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .capability_grants_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn capability_grants_revoke(
        &self,
        input: &rpc::CapabilityGrantsRevokeInput,
    ) -> Result<
        rpc::CapabilityGrantsRevokeOutput,
        trellis_rs::client::CallError<rpc::CapabilityGrantsRevokeError>,
    > {
        self.inner.call::<rpc::CapabilityGrantsRevoke>(input).await
    }
    pub async fn connections_kick(
        &self,
        input: &rpc::ConnectionsKickInput,
    ) -> Result<rpc::ConnectionsKickOutput, trellis_rs::client::CallError<rpc::ConnectionsKickError>>
    {
        self.inner.call::<rpc::ConnectionsKick>(input).await
    }
    pub async fn connections_list(
        &self,
        input: &rpc::ConnectionsListInput,
    ) -> Result<rpc::ConnectionsListOutput, trellis_rs::client::CallError<rpc::ConnectionsListError>>
    {
        self.inner.call::<rpc::ConnectionsList>(input).await
    }
    pub fn connections_list_pages(
        &self,
        input: rpc::ConnectionsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ConnectionsListOutput, crate::PaginationError<rpc::ConnectionsListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .connections_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn connections_list_items(
        &self,
        input: rpc::ConnectionsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthAttachment,
            crate::PaginationError<rpc::ConnectionsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthAttachment>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .connections_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn delegations_list(
        &self,
        input: &rpc::DelegationsListInput,
    ) -> Result<rpc::DelegationsListOutput, trellis_rs::client::CallError<rpc::DelegationsListError>>
    {
        self.inner.call::<rpc::DelegationsList>(input).await
    }
    pub fn delegations_list_pages(
        &self,
        input: rpc::DelegationsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::DelegationsListOutput, crate::PaginationError<rpc::DelegationsListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .delegations_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn delegations_list_items(
        &self,
        input: rpc::DelegationsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthDelegation,
            crate::PaginationError<rpc::DelegationsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthDelegation>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .delegations_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn delegations_review(
        &self,
        input: &rpc::DelegationsReviewInput,
    ) -> Result<
        rpc::DelegationsReviewOutput,
        trellis_rs::client::CallError<rpc::DelegationsReviewError>,
    > {
        self.inner.call::<rpc::DelegationsReview>(input).await
    }
    pub async fn delegations_revoke(
        &self,
        input: &rpc::DelegationsRevokeInput,
    ) -> Result<
        rpc::DelegationsRevokeOutput,
        trellis_rs::client::CallError<rpc::DelegationsRevokeError>,
    > {
        self.inner.call::<rpc::DelegationsRevoke>(input).await
    }
    pub async fn deployments_apply(
        &self,
        input: &rpc::DeploymentsApplyInput,
    ) -> Result<
        rpc::DeploymentsApplyOutput,
        trellis_rs::client::CallError<rpc::DeploymentsApplyError>,
    > {
        self.inner.call::<rpc::DeploymentsApply>(input).await
    }
    pub async fn deployments_create(
        &self,
        input: &rpc::DeploymentsCreateInput,
    ) -> Result<
        rpc::DeploymentsCreateOutput,
        trellis_rs::client::CallError<rpc::DeploymentsCreateError>,
    > {
        self.inner.call::<rpc::DeploymentsCreate>(input).await
    }
    pub async fn deployments_disable(
        &self,
        input: &rpc::DeploymentsDisableInput,
    ) -> Result<
        rpc::DeploymentsDisableOutput,
        trellis_rs::client::CallError<rpc::DeploymentsDisableError>,
    > {
        self.inner.call::<rpc::DeploymentsDisable>(input).await
    }
    pub async fn deployments_enable(
        &self,
        input: &rpc::DeploymentsEnableInput,
    ) -> Result<
        rpc::DeploymentsEnableOutput,
        trellis_rs::client::CallError<rpc::DeploymentsEnableError>,
    > {
        self.inner.call::<rpc::DeploymentsEnable>(input).await
    }
    pub async fn deployments_get(
        &self,
        input: &rpc::DeploymentsGetInput,
    ) -> Result<rpc::DeploymentsGetOutput, trellis_rs::client::CallError<rpc::DeploymentsGetError>>
    {
        self.inner.call::<rpc::DeploymentsGet>(input).await
    }
    pub async fn deployments_list(
        &self,
        input: &rpc::DeploymentsListInput,
    ) -> Result<rpc::DeploymentsListOutput, trellis_rs::client::CallError<rpc::DeploymentsListError>>
    {
        self.inner.call::<rpc::DeploymentsList>(input).await
    }
    pub fn deployments_list_pages(
        &self,
        input: rpc::DeploymentsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::DeploymentsListOutput, crate::PaginationError<rpc::DeploymentsListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .deployments_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn deployments_list_items(
        &self,
        input: rpc::DeploymentsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthDeployment,
            crate::PaginationError<rpc::DeploymentsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthDeployment>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .deployments_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn deployments_remove(
        &self,
        input: &rpc::DeploymentsRemoveInput,
    ) -> Result<
        rpc::DeploymentsRemoveOutput,
        trellis_rs::client::CallError<rpc::DeploymentsRemoveError>,
    > {
        self.inner.call::<rpc::DeploymentsRemove>(input).await
    }
    pub async fn devices_disable(
        &self,
        input: &rpc::DevicesDisableInput,
    ) -> Result<rpc::DevicesDisableOutput, trellis_rs::client::CallError<rpc::DevicesDisableError>>
    {
        self.inner.call::<rpc::DevicesDisable>(input).await
    }
    pub async fn devices_enable(
        &self,
        input: &rpc::DevicesEnableInput,
    ) -> Result<rpc::DevicesEnableOutput, trellis_rs::client::CallError<rpc::DevicesEnableError>>
    {
        self.inner.call::<rpc::DevicesEnable>(input).await
    }
    pub async fn devices_list(
        &self,
        input: &rpc::DevicesListInput,
    ) -> Result<rpc::DevicesListOutput, trellis_rs::client::CallError<rpc::DevicesListError>> {
        self.inner.call::<rpc::DevicesList>(input).await
    }
    pub fn devices_list_pages(
        &self,
        input: rpc::DevicesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::DevicesListOutput, crate::PaginationError<rpc::DevicesListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .devices_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn devices_list_items(
        &self,
        input: rpc::DevicesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthProvisionedInstance,
            crate::PaginationError<rpc::DevicesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthProvisionedInstance>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .devices_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn devices_provision(
        &self,
        input: &rpc::DevicesProvisionInput,
    ) -> Result<
        rpc::DevicesProvisionOutput,
        trellis_rs::client::CallError<rpc::DevicesProvisionError>,
    > {
        self.inner.call::<rpc::DevicesProvision>(input).await
    }
    pub async fn devices_remove(
        &self,
        input: &rpc::DevicesRemoveInput,
    ) -> Result<rpc::DevicesRemoveOutput, trellis_rs::client::CallError<rpc::DevicesRemoveError>>
    {
        self.inner.call::<rpc::DevicesRemove>(input).await
    }
    pub async fn issuers_resolve_chain(
        &self,
        input: &rpc::IssuersResolveChainInput,
    ) -> Result<
        rpc::IssuersResolveChainOutput,
        trellis_rs::client::CallError<rpc::IssuersResolveChainError>,
    > {
        self.inner.call::<rpc::IssuersResolveChain>(input).await
    }
    pub async fn issuers_revoke(
        &self,
        input: &rpc::IssuersRevokeInput,
    ) -> Result<rpc::IssuersRevokeOutput, trellis_rs::client::CallError<rpc::IssuersRevokeError>>
    {
        self.inner.call::<rpc::IssuersRevoke>(input).await
    }
    pub async fn issuers_rotate(
        &self,
        input: &rpc::IssuersRotateInput,
    ) -> Result<rpc::IssuersRotateOutput, trellis_rs::client::CallError<rpc::IssuersRotateError>>
    {
        self.inner.call::<rpc::IssuersRotate>(input).await
    }
    pub async fn o_auth_clients_delete(
        &self,
        input: &rpc::OAuthClientsDeleteInput,
    ) -> Result<
        rpc::OAuthClientsDeleteOutput,
        trellis_rs::client::CallError<rpc::OAuthClientsDeleteError>,
    > {
        self.inner.call::<rpc::OAuthClientsDelete>(input).await
    }
    pub async fn o_auth_clients_disable(
        &self,
        input: &rpc::OAuthClientsDisableInput,
    ) -> Result<
        rpc::OAuthClientsDisableOutput,
        trellis_rs::client::CallError<rpc::OAuthClientsDisableError>,
    > {
        self.inner.call::<rpc::OAuthClientsDisable>(input).await
    }
    pub async fn o_auth_clients_get(
        &self,
        input: &rpc::OAuthClientsGetInput,
    ) -> Result<rpc::OAuthClientsGetOutput, trellis_rs::client::CallError<rpc::OAuthClientsGetError>>
    {
        self.inner.call::<rpc::OAuthClientsGet>(input).await
    }
    pub async fn o_auth_clients_list(
        &self,
        input: &rpc::OAuthClientsListInput,
    ) -> Result<
        rpc::OAuthClientsListOutput,
        trellis_rs::client::CallError<rpc::OAuthClientsListError>,
    > {
        self.inner.call::<rpc::OAuthClientsList>(input).await
    }
    pub fn o_auth_clients_list_pages(
        &self,
        input: rpc::OAuthClientsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::OAuthClientsListOutput, crate::PaginationError<rpc::OAuthClientsListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .o_auth_clients_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn o_auth_clients_list_items(
        &self,
        input: rpc::OAuthClientsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthOAuthClient,
            crate::PaginationError<rpc::OAuthClientsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthOAuthClient>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .o_auth_clients_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn o_auth_clients_put(
        &self,
        input: &rpc::OAuthClientsPutInput,
    ) -> Result<rpc::OAuthClientsPutOutput, trellis_rs::client::CallError<rpc::OAuthClientsPutError>>
    {
        self.inner.call::<rpc::OAuthClientsPut>(input).await
    }
    pub async fn oidc_providers_delete(
        &self,
        input: &rpc::OIDCProvidersDeleteInput,
    ) -> Result<
        rpc::OIDCProvidersDeleteOutput,
        trellis_rs::client::CallError<rpc::OIDCProvidersDeleteError>,
    > {
        self.inner.call::<rpc::OIDCProvidersDelete>(input).await
    }
    pub async fn oidc_providers_disable(
        &self,
        input: &rpc::OIDCProvidersDisableInput,
    ) -> Result<
        rpc::OIDCProvidersDisableOutput,
        trellis_rs::client::CallError<rpc::OIDCProvidersDisableError>,
    > {
        self.inner.call::<rpc::OIDCProvidersDisable>(input).await
    }
    pub async fn oidc_providers_get(
        &self,
        input: &rpc::OIDCProvidersGetInput,
    ) -> Result<
        rpc::OIDCProvidersGetOutput,
        trellis_rs::client::CallError<rpc::OIDCProvidersGetError>,
    > {
        self.inner.call::<rpc::OIDCProvidersGet>(input).await
    }
    pub async fn oidc_providers_list(
        &self,
        input: &rpc::OIDCProvidersListInput,
    ) -> Result<
        rpc::OIDCProvidersListOutput,
        trellis_rs::client::CallError<rpc::OIDCProvidersListError>,
    > {
        self.inner.call::<rpc::OIDCProvidersList>(input).await
    }
    pub fn oidc_providers_list_pages(
        &self,
        input: rpc::OIDCProvidersListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::OIDCProvidersListOutput, crate::PaginationError<rpc::OIDCProvidersListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .oidc_providers_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn oidc_providers_list_items(
        &self,
        input: rpc::OIDCProvidersListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthOIDCProvider,
            crate::PaginationError<rpc::OIDCProvidersListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthOIDCProvider>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .oidc_providers_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn oidc_providers_put(
        &self,
        input: &rpc::OIDCProvidersPutInput,
    ) -> Result<
        rpc::OIDCProvidersPutOutput,
        trellis_rs::client::CallError<rpc::OIDCProvidersPutError>,
    > {
        self.inner.call::<rpc::OIDCProvidersPut>(input).await
    }
    pub async fn oidc_role_mappings_delete(
        &self,
        input: &rpc::OIDCRoleMappingsDeleteInput,
    ) -> Result<
        rpc::OIDCRoleMappingsDeleteOutput,
        trellis_rs::client::CallError<rpc::OIDCRoleMappingsDeleteError>,
    > {
        self.inner.call::<rpc::OIDCRoleMappingsDelete>(input).await
    }
    pub async fn oidc_role_mappings_list(
        &self,
        input: &rpc::OIDCRoleMappingsListInput,
    ) -> Result<
        rpc::OIDCRoleMappingsListOutput,
        trellis_rs::client::CallError<rpc::OIDCRoleMappingsListError>,
    > {
        self.inner.call::<rpc::OIDCRoleMappingsList>(input).await
    }
    pub fn oidc_role_mappings_list_pages(
        &self,
        input: rpc::OIDCRoleMappingsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::OIDCRoleMappingsListOutput,
            crate::PaginationError<rpc::OIDCRoleMappingsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .oidc_role_mappings_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn oidc_role_mappings_list_items(
        &self,
        input: rpc::OIDCRoleMappingsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthOIDCRoleMapping,
            crate::PaginationError<rpc::OIDCRoleMappingsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthOIDCRoleMapping>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .oidc_role_mappings_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn oidc_role_mappings_put(
        &self,
        input: &rpc::OIDCRoleMappingsPutInput,
    ) -> Result<
        rpc::OIDCRoleMappingsPutOutput,
        trellis_rs::client::CallError<rpc::OIDCRoleMappingsPutError>,
    > {
        self.inner.call::<rpc::OIDCRoleMappingsPut>(input).await
    }
    pub async fn platform_delegations_list(
        &self,
        input: &rpc::PlatformDelegationsListInput,
    ) -> Result<
        rpc::PlatformDelegationsListOutput,
        trellis_rs::client::CallError<rpc::PlatformDelegationsListError>,
    > {
        self.inner.call::<rpc::PlatformDelegationsList>(input).await
    }
    pub fn platform_delegations_list_pages(
        &self,
        input: rpc::PlatformDelegationsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::PlatformDelegationsListOutput,
            crate::PaginationError<rpc::PlatformDelegationsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .platform_delegations_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn platform_delegations_list_items(
        &self,
        input: rpc::PlatformDelegationsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthPlatformDelegation,
            crate::PaginationError<rpc::PlatformDelegationsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthPlatformDelegation>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .platform_delegations_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn platform_delegations_put(
        &self,
        input: &rpc::PlatformDelegationsPutInput,
    ) -> Result<
        rpc::PlatformDelegationsPutOutput,
        trellis_rs::client::CallError<rpc::PlatformDelegationsPutError>,
    > {
        self.inner.call::<rpc::PlatformDelegationsPut>(input).await
    }
    pub async fn platform_delegations_revoke(
        &self,
        input: &rpc::PlatformDelegationsRevokeInput,
    ) -> Result<
        rpc::PlatformDelegationsRevokeOutput,
        trellis_rs::client::CallError<rpc::PlatformDelegationsRevokeError>,
    > {
        self.inner
            .call::<rpc::PlatformDelegationsRevoke>(input)
            .await
    }
    pub async fn platform_privileges_assign(
        &self,
        input: &rpc::PlatformPrivilegesAssignInput,
    ) -> Result<
        rpc::PlatformPrivilegesAssignOutput,
        trellis_rs::client::CallError<rpc::PlatformPrivilegesAssignError>,
    > {
        self.inner
            .call::<rpc::PlatformPrivilegesAssign>(input)
            .await
    }
    pub async fn platform_privileges_list(
        &self,
        input: &rpc::PlatformPrivilegesListInput,
    ) -> Result<
        rpc::PlatformPrivilegesListOutput,
        trellis_rs::client::CallError<rpc::PlatformPrivilegesListError>,
    > {
        self.inner.call::<rpc::PlatformPrivilegesList>(input).await
    }
    pub fn platform_privileges_list_pages(
        &self,
        input: rpc::PlatformPrivilegesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::PlatformPrivilegesListOutput,
            crate::PaginationError<rpc::PlatformPrivilegesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .platform_privileges_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn platform_privileges_list_items(
        &self,
        input: rpc::PlatformPrivilegesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthPlatformAssignment,
            crate::PaginationError<rpc::PlatformPrivilegesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthPlatformAssignment>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .platform_privileges_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn platform_privileges_revoke(
        &self,
        input: &rpc::PlatformPrivilegesRevokeInput,
    ) -> Result<
        rpc::PlatformPrivilegesRevokeOutput,
        trellis_rs::client::CallError<rpc::PlatformPrivilegesRevokeError>,
    > {
        self.inner
            .call::<rpc::PlatformPrivilegesRevoke>(input)
            .await
    }
    pub async fn principals_effective_access(
        &self,
        input: &rpc::PrincipalsEffectiveAccessInput,
    ) -> Result<
        rpc::PrincipalsEffectiveAccessOutput,
        trellis_rs::client::CallError<rpc::PrincipalsEffectiveAccessError>,
    > {
        self.inner
            .call::<rpc::PrincipalsEffectiveAccess>(input)
            .await
    }
    pub async fn resources_get(
        &self,
        input: &rpc::ResourcesGetInput,
    ) -> Result<rpc::ResourcesGetOutput, trellis_rs::client::CallError<rpc::ResourcesGetError>>
    {
        self.inner.call::<rpc::ResourcesGet>(input).await
    }
    pub async fn resources_list(
        &self,
        input: &rpc::ResourcesListInput,
    ) -> Result<rpc::ResourcesListOutput, trellis_rs::client::CallError<rpc::ResourcesListError>>
    {
        self.inner.call::<rpc::ResourcesList>(input).await
    }
    pub fn resources_list_pages(
        &self,
        input: rpc::ResourcesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ResourcesListOutput, crate::PaginationError<rpc::ResourcesListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .resources_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn resources_list_items(
        &self,
        input: rpc::ResourcesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthResourceBinding,
            crate::PaginationError<rpc::ResourcesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthResourceBinding>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .resources_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn resources_remove(
        &self,
        input: &rpc::ResourcesRemoveInput,
    ) -> Result<rpc::ResourcesRemoveOutput, trellis_rs::client::CallError<rpc::ResourcesRemoveError>>
    {
        self.inner.call::<rpc::ResourcesRemove>(input).await
    }
    pub async fn role_assignments_assign(
        &self,
        input: &rpc::RoleAssignmentsAssignInput,
    ) -> Result<
        rpc::RoleAssignmentsAssignOutput,
        trellis_rs::client::CallError<rpc::RoleAssignmentsAssignError>,
    > {
        self.inner.call::<rpc::RoleAssignmentsAssign>(input).await
    }
    pub async fn role_assignments_list(
        &self,
        input: &rpc::RoleAssignmentsListInput,
    ) -> Result<
        rpc::RoleAssignmentsListOutput,
        trellis_rs::client::CallError<rpc::RoleAssignmentsListError>,
    > {
        self.inner.call::<rpc::RoleAssignmentsList>(input).await
    }
    pub fn role_assignments_list_pages(
        &self,
        input: rpc::RoleAssignmentsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::RoleAssignmentsListOutput,
            crate::PaginationError<rpc::RoleAssignmentsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .role_assignments_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn role_assignments_list_items(
        &self,
        input: rpc::RoleAssignmentsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthRoleAssignment,
            crate::PaginationError<rpc::RoleAssignmentsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthRoleAssignment>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .role_assignments_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn role_assignments_revoke(
        &self,
        input: &rpc::RoleAssignmentsRevokeInput,
    ) -> Result<
        rpc::RoleAssignmentsRevokeOutput,
        trellis_rs::client::CallError<rpc::RoleAssignmentsRevokeError>,
    > {
        self.inner.call::<rpc::RoleAssignmentsRevoke>(input).await
    }
    pub async fn roles_delete(
        &self,
        input: &rpc::RolesDeleteInput,
    ) -> Result<rpc::RolesDeleteOutput, trellis_rs::client::CallError<rpc::RolesDeleteError>> {
        self.inner.call::<rpc::RolesDelete>(input).await
    }
    pub async fn roles_get(
        &self,
        input: &rpc::RolesGetInput,
    ) -> Result<rpc::RolesGetOutput, trellis_rs::client::CallError<rpc::RolesGetError>> {
        self.inner.call::<rpc::RolesGet>(input).await
    }
    pub async fn roles_list(
        &self,
        input: &rpc::RolesListInput,
    ) -> Result<rpc::RolesListOutput, trellis_rs::client::CallError<rpc::RolesListError>> {
        self.inner.call::<rpc::RolesList>(input).await
    }
    pub fn roles_list_pages(
        &self,
        input: rpc::RolesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::RolesListOutput, crate::PaginationError<rpc::RolesListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .roles_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn roles_list_items(
        &self,
        input: rpc::RolesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<crate::__types::trellis::AuthRole, crate::PaginationError<rpc::RolesListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthRole>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .roles_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn roles_put(
        &self,
        input: &rpc::RolesPutInput,
    ) -> Result<rpc::RolesPutOutput, trellis_rs::client::CallError<rpc::RolesPutError>> {
        self.inner.call::<rpc::RolesPut>(input).await
    }
    pub async fn service_instances_disable(
        &self,
        input: &rpc::ServiceInstancesDisableInput,
    ) -> Result<
        rpc::ServiceInstancesDisableOutput,
        trellis_rs::client::CallError<rpc::ServiceInstancesDisableError>,
    > {
        self.inner.call::<rpc::ServiceInstancesDisable>(input).await
    }
    pub async fn service_instances_enable(
        &self,
        input: &rpc::ServiceInstancesEnableInput,
    ) -> Result<
        rpc::ServiceInstancesEnableOutput,
        trellis_rs::client::CallError<rpc::ServiceInstancesEnableError>,
    > {
        self.inner.call::<rpc::ServiceInstancesEnable>(input).await
    }
    pub async fn service_instances_list(
        &self,
        input: &rpc::ServiceInstancesListInput,
    ) -> Result<
        rpc::ServiceInstancesListOutput,
        trellis_rs::client::CallError<rpc::ServiceInstancesListError>,
    > {
        self.inner.call::<rpc::ServiceInstancesList>(input).await
    }
    pub fn service_instances_list_pages(
        &self,
        input: rpc::ServiceInstancesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::ServiceInstancesListOutput,
            crate::PaginationError<rpc::ServiceInstancesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .service_instances_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn service_instances_list_items(
        &self,
        input: rpc::ServiceInstancesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthProvisionedInstance,
            crate::PaginationError<rpc::ServiceInstancesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthProvisionedInstance>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .service_instances_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn service_instances_provision(
        &self,
        input: &rpc::ServiceInstancesProvisionInput,
    ) -> Result<
        rpc::ServiceInstancesProvisionOutput,
        trellis_rs::client::CallError<rpc::ServiceInstancesProvisionError>,
    > {
        self.inner
            .call::<rpc::ServiceInstancesProvision>(input)
            .await
    }
    pub async fn service_instances_remove(
        &self,
        input: &rpc::ServiceInstancesRemoveInput,
    ) -> Result<
        rpc::ServiceInstancesRemoveOutput,
        trellis_rs::client::CallError<rpc::ServiceInstancesRemoveError>,
    > {
        self.inner.call::<rpc::ServiceInstancesRemove>(input).await
    }
    pub async fn sessions_list(
        &self,
        input: &rpc::SessionsListInput,
    ) -> Result<rpc::SessionsListOutput, trellis_rs::client::CallError<rpc::SessionsListError>>
    {
        self.inner.call::<rpc::SessionsList>(input).await
    }
    pub fn sessions_list_pages(
        &self,
        input: rpc::SessionsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::SessionsListOutput, crate::PaginationError<rpc::SessionsListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .sessions_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn sessions_list_items(
        &self,
        input: rpc::SessionsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthLoginSession,
            crate::PaginationError<rpc::SessionsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthLoginSession>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .sessions_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn sessions_logout(
        &self,
        input: &rpc::SessionsLogoutInput,
    ) -> Result<rpc::SessionsLogoutOutput, trellis_rs::client::CallError<rpc::SessionsLogoutError>>
    {
        self.inner.call::<rpc::SessionsLogout>(input).await
    }
    pub async fn sessions_me(
        &self,
        input: &rpc::SessionsMeInput,
    ) -> Result<rpc::SessionsMeOutput, trellis_rs::client::CallError<rpc::SessionsMeError>> {
        self.inner.call::<rpc::SessionsMe>(input).await
    }
    pub async fn sessions_revoke(
        &self,
        input: &rpc::SessionsRevokeInput,
    ) -> Result<rpc::SessionsRevokeOutput, trellis_rs::client::CallError<rpc::SessionsRevokeError>>
    {
        self.inner.call::<rpc::SessionsRevoke>(input).await
    }
    pub async fn user_identities_list(
        &self,
        input: &rpc::UserIdentitiesListInput,
    ) -> Result<
        rpc::UserIdentitiesListOutput,
        trellis_rs::client::CallError<rpc::UserIdentitiesListError>,
    > {
        self.inner.call::<rpc::UserIdentitiesList>(input).await
    }
    pub fn user_identities_list_pages(
        &self,
        input: rpc::UserIdentitiesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::UserIdentitiesListOutput, crate::PaginationError<rpc::UserIdentitiesListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .user_identities_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn user_identities_list_items(
        &self,
        input: rpc::UserIdentitiesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthUserIdentity,
            crate::PaginationError<rpc::UserIdentitiesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthUserIdentity>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .user_identities_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn user_identities_unlink(
        &self,
        input: &rpc::UserIdentitiesUnlinkInput,
    ) -> Result<
        rpc::UserIdentitiesUnlinkOutput,
        trellis_rs::client::CallError<rpc::UserIdentitiesUnlinkError>,
    > {
        self.inner.call::<rpc::UserIdentitiesUnlink>(input).await
    }
    pub async fn users_create(
        &self,
        input: &rpc::UsersCreateInput,
    ) -> Result<rpc::UsersCreateOutput, trellis_rs::client::CallError<rpc::UsersCreateError>> {
        self.inner.call::<rpc::UsersCreate>(input).await
    }
    pub async fn users_get(
        &self,
        input: &rpc::UsersGetInput,
    ) -> Result<rpc::UsersGetOutput, trellis_rs::client::CallError<rpc::UsersGetError>> {
        self.inner.call::<rpc::UsersGet>(input).await
    }
    pub async fn users_identity_link_create(
        &self,
        input: &rpc::UsersIdentityLinkCreateInput,
    ) -> Result<
        rpc::UsersIdentityLinkCreateOutput,
        trellis_rs::client::CallError<rpc::UsersIdentityLinkCreateError>,
    > {
        self.inner.call::<rpc::UsersIdentityLinkCreate>(input).await
    }
    pub async fn users_list(
        &self,
        input: &rpc::UsersListInput,
    ) -> Result<rpc::UsersListOutput, trellis_rs::client::CallError<rpc::UsersListError>> {
        self.inner.call::<rpc::UsersList>(input).await
    }
    pub fn users_list_pages(
        &self,
        input: rpc::UsersListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::UsersListOutput, crate::PaginationError<rpc::UsersListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (client, input, seen, false),
            |(client, mut input, mut seen, done)| async move {
                if done {
                    return Ok(None);
                }
                let page = client
                    .users_list(&input)
                    .await
                    .map_err(crate::PaginationError::Call)?;
                let next = page.page.next_cursor.clone();
                let done = next.is_none();
                if let Some(cursor) = next {
                    if !seen.insert(cursor.clone()) {
                        return Err(crate::PaginationError::RepeatedCursor(cursor));
                    }
                    let limit = input.page.as_ref().and_then(|page| page.limit);
                    input.page = Some(crate::__types::CursorQuery {
                        cursor: Some(cursor),
                        limit,
                    });
                }
                Ok(Some((page, (client, input, seen, done))))
            },
        ))
    }
    pub fn users_list_items(
        &self,
        input: rpc::UsersListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<crate::__types::trellis::AuthUser, crate::PaginationError<rpc::UsersListError>>,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(futures_util::stream::try_unfold(
            (
                client,
                input,
                seen,
                false,
                Vec::<crate::__types::trellis::AuthUser>::new().into_iter(),
            ),
            |(client, mut input, mut seen, mut done, mut items)| async move {
                loop {
                    if let Some(item) = items.next() {
                        return Ok(Some((item, (client, input, seen, done, items))));
                    }
                    if done {
                        return Ok(None);
                    }
                    let page = client
                        .users_list(&input)
                        .await
                        .map_err(crate::PaginationError::Call)?;
                    let next = page.page.next_cursor.clone();
                    done = next.is_none();
                    if let Some(cursor) = next {
                        if !seen.insert(cursor.clone()) {
                            return Err(crate::PaginationError::RepeatedCursor(cursor));
                        }
                        let limit = input.page.as_ref().and_then(|page| page.limit);
                        input.page = Some(crate::__types::CursorQuery {
                            cursor: Some(cursor),
                            limit,
                        });
                    }
                    items = page.items.into_iter();
                }
            },
        ))
    }
    pub async fn users_password_change(
        &self,
        input: &rpc::UsersPasswordChangeInput,
    ) -> Result<
        rpc::UsersPasswordChangeOutput,
        trellis_rs::client::CallError<rpc::UsersPasswordChangeError>,
    > {
        self.inner.call::<rpc::UsersPasswordChange>(input).await
    }
    pub async fn users_password_reset_create(
        &self,
        input: &rpc::UsersPasswordResetCreateInput,
    ) -> Result<
        rpc::UsersPasswordResetCreateOutput,
        trellis_rs::client::CallError<rpc::UsersPasswordResetCreateError>,
    > {
        self.inner
            .call::<rpc::UsersPasswordResetCreate>(input)
            .await
    }
    pub async fn users_resolve(
        &self,
        input: &rpc::UsersResolveInput,
    ) -> Result<rpc::UsersResolveOutput, trellis_rs::client::CallError<rpc::UsersResolveError>>
    {
        self.inner.call::<rpc::UsersResolve>(input).await
    }
    pub async fn users_update(
        &self,
        input: &rpc::UsersUpdateInput,
    ) -> Result<rpc::UsersUpdateOutput, trellis_rs::client::CallError<rpc::UsersUpdateError>> {
        self.inner.call::<rpc::UsersUpdate>(input).await
    }
    pub async fn publish_authorization_sessions_retired(
        &self,
        event: &events::AuthorizationSessionsRetiredEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner
            .publish::<events::AuthorizationSessionsRetired>(event)
            .await
    }
    pub async fn subscribe_authorization_sessions_retired(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<
                events::AuthorizationSessionsRetiredEvent,
                trellis_rs::client::TrellisClientError,
            >,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::AuthorizationSessionsRetired>(options)
            .await
    }
    pub async fn publish_connections_closed(
        &self,
        event: &events::ConnectionsClosedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::ConnectionsClosed>(event).await
    }
    pub async fn subscribe_connections_closed(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::ConnectionsClosedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::ConnectionsClosed>(options)
            .await
    }
    pub async fn publish_connections_kicked(
        &self,
        event: &events::ConnectionsKickedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::ConnectionsKicked>(event).await
    }
    pub async fn subscribe_connections_kicked(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::ConnectionsKickedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::ConnectionsKicked>(options)
            .await
    }
    pub async fn publish_connections_opened(
        &self,
        event: &events::ConnectionsOpenedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::ConnectionsOpened>(event).await
    }
    pub async fn subscribe_connections_opened(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::ConnectionsOpenedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::ConnectionsOpened>(options)
            .await
    }
    pub async fn publish_issuers_revoked(
        &self,
        event: &events::IssuersRevokedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::IssuersRevoked>(event).await
    }
    pub async fn subscribe_issuers_revoked(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::IssuersRevokedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::IssuersRevoked>(options)
            .await
    }
    pub async fn publish_issuers_rotated(
        &self,
        event: &events::IssuersRotatedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::IssuersRotated>(event).await
    }
    pub async fn subscribe_issuers_rotated(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::IssuersRotatedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::IssuersRotated>(options)
            .await
    }
}
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>) -> Self {
        Self { runtime }
    }
    pub fn register_admission_renew<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::AdmissionRenewInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::AdmissionRenewOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::AdmissionRenew, _, _>(handler);
    }
    pub fn register_apis_accept<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ApisAcceptInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::ApisAcceptOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::ApisAccept, _, _>(handler);
    }
    pub fn register_apis_force_replace<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ApisForceReplaceInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ApisForceReplaceOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ApisForceReplace, _, _>(handler);
    }
    pub fn register_apis_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ApisGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::ApisGetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::ApisGet, _, _>(handler);
    }
    pub fn register_apis_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ApisListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::ApisListOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::ApisList, _, _>(handler);
    }
    pub fn register_apis_review<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ApisReviewInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::ApisReviewOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::ApisReview, _, _>(handler);
    }
    pub fn register_authority_renew<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::AuthorityRenewInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::AuthorityRenewOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::AuthorityRenew, _, _>(handler);
    }
    pub fn register_authority_resolve<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::AuthorityResolveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::AuthorityResolveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::AuthorityResolve, _, _>(handler);
    }
    pub fn register_authority_status<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::AuthorityStatusInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::AuthorityStatusOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::AuthorityStatus, _, _>(handler);
    }
    pub fn register_authorization_sessions_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::AuthorizationSessionsListInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::AuthorizationSessionsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::AuthorizationSessionsList, _, _>(handler);
    }
    pub fn register_authorization_sessions_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::AuthorizationSessionsRevokeInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::AuthorizationSessionsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::AuthorizationSessionsRevoke, _, _>(handler);
    }
    pub fn register_capabilities_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilitiesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilitiesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilitiesList, _, _>(handler);
    }
    pub fn register_capability_grants_grant<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGrantsGrantInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGrantsGrantOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGrantsGrant, _, _>(handler);
    }
    pub fn register_capability_grants_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGrantsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGrantsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGrantsList, _, _>(handler);
    }
    pub fn register_capability_grants_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGrantsRevokeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGrantsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGrantsRevoke, _, _>(handler);
    }
    pub fn register_connections_kick<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ConnectionsKickInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ConnectionsKickOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ConnectionsKick, _, _>(handler);
    }
    pub fn register_connections_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ConnectionsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ConnectionsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ConnectionsList, _, _>(handler);
    }
    pub fn register_delegations_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DelegationsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DelegationsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DelegationsList, _, _>(handler);
    }
    pub fn register_delegations_review<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DelegationsReviewInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DelegationsReviewOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DelegationsReview, _, _>(handler);
    }
    pub fn register_delegations_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DelegationsRevokeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DelegationsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DelegationsRevoke, _, _>(handler);
    }
    pub fn register_deployments_apply<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsApplyInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsApplyOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsApply, _, _>(handler);
    }
    pub fn register_deployments_create<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsCreateInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsCreateOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsCreate, _, _>(handler);
    }
    pub fn register_deployments_disable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsDisableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsDisableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsDisable, _, _>(handler);
    }
    pub fn register_deployments_enable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsEnableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsEnableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsEnable, _, _>(handler);
    }
    pub fn register_deployments_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsGet, _, _>(handler);
    }
    pub fn register_deployments_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsList, _, _>(handler);
    }
    pub fn register_deployments_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeploymentsRemoveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeploymentsRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeploymentsRemove, _, _>(handler);
    }
    pub fn register_devices_disable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DevicesDisableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DevicesDisableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DevicesDisable, _, _>(handler);
    }
    pub fn register_devices_enable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DevicesEnableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DevicesEnableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DevicesEnable, _, _>(handler);
    }
    pub fn register_devices_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DevicesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::DevicesListOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::DevicesList, _, _>(handler);
    }
    pub fn register_devices_provision<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DevicesProvisionInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DevicesProvisionOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DevicesProvision, _, _>(handler);
    }
    pub fn register_devices_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DevicesRemoveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DevicesRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DevicesRemove, _, _>(handler);
    }
    pub fn register_issuers_resolve_chain<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::IssuersResolveChainInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::IssuersResolveChainOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::IssuersResolveChain, _, _>(handler);
    }
    pub fn register_issuers_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::IssuersRevokeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::IssuersRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::IssuersRevoke, _, _>(handler);
    }
    pub fn register_issuers_rotate<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::IssuersRotateInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::IssuersRotateOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::IssuersRotate, _, _>(handler);
    }
    pub fn register_o_auth_clients_delete<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OAuthClientsDeleteInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OAuthClientsDeleteOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OAuthClientsDelete, _, _>(handler);
    }
    pub fn register_o_auth_clients_disable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OAuthClientsDisableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OAuthClientsDisableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OAuthClientsDisable, _, _>(handler);
    }
    pub fn register_o_auth_clients_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OAuthClientsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OAuthClientsGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OAuthClientsGet, _, _>(handler);
    }
    pub fn register_o_auth_clients_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OAuthClientsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OAuthClientsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OAuthClientsList, _, _>(handler);
    }
    pub fn register_o_auth_clients_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OAuthClientsPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OAuthClientsPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OAuthClientsPut, _, _>(handler);
    }
    pub fn register_oidc_providers_delete<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCProvidersDeleteInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCProvidersDeleteOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCProvidersDelete, _, _>(handler);
    }
    pub fn register_oidc_providers_disable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCProvidersDisableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCProvidersDisableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCProvidersDisable, _, _>(handler);
    }
    pub fn register_oidc_providers_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCProvidersGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCProvidersGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCProvidersGet, _, _>(handler);
    }
    pub fn register_oidc_providers_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCProvidersListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCProvidersListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCProvidersList, _, _>(handler);
    }
    pub fn register_oidc_providers_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCProvidersPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCProvidersPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCProvidersPut, _, _>(handler);
    }
    pub fn register_oidc_role_mappings_delete<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCRoleMappingsDeleteInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCRoleMappingsDeleteOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCRoleMappingsDelete, _, _>(handler);
    }
    pub fn register_oidc_role_mappings_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCRoleMappingsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCRoleMappingsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCRoleMappingsList, _, _>(handler);
    }
    pub fn register_oidc_role_mappings_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::OIDCRoleMappingsPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::OIDCRoleMappingsPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::OIDCRoleMappingsPut, _, _>(handler);
    }
    pub fn register_platform_delegations_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PlatformDelegationsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PlatformDelegationsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PlatformDelegationsList, _, _>(handler);
    }
    pub fn register_platform_delegations_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PlatformDelegationsPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PlatformDelegationsPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PlatformDelegationsPut, _, _>(handler);
    }
    pub fn register_platform_delegations_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PlatformDelegationsRevokeInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PlatformDelegationsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PlatformDelegationsRevoke, _, _>(handler);
    }
    pub fn register_platform_privileges_assign<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PlatformPrivilegesAssignInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PlatformPrivilegesAssignOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PlatformPrivilegesAssign, _, _>(handler);
    }
    pub fn register_platform_privileges_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PlatformPrivilegesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PlatformPrivilegesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PlatformPrivilegesList, _, _>(handler);
    }
    pub fn register_platform_privileges_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PlatformPrivilegesRevokeInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PlatformPrivilegesRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PlatformPrivilegesRevoke, _, _>(handler);
    }
    pub fn register_principals_effective_access<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PrincipalsEffectiveAccessInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PrincipalsEffectiveAccessOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PrincipalsEffectiveAccess, _, _>(handler);
    }
    pub fn register_resources_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ResourcesGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ResourcesGet, _, _>(handler);
    }
    pub fn register_resources_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ResourcesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ResourcesList, _, _>(handler);
    }
    pub fn register_resources_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ResourcesRemoveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ResourcesRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ResourcesRemove, _, _>(handler);
    }
    pub fn register_role_assignments_assign<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RoleAssignmentsAssignInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::RoleAssignmentsAssignOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::RoleAssignmentsAssign, _, _>(handler);
    }
    pub fn register_role_assignments_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RoleAssignmentsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::RoleAssignmentsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::RoleAssignmentsList, _, _>(handler);
    }
    pub fn register_role_assignments_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RoleAssignmentsRevokeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::RoleAssignmentsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::RoleAssignmentsRevoke, _, _>(handler);
    }
    pub fn register_roles_delete<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RolesDeleteInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::RolesDeleteOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::RolesDelete, _, _>(handler);
    }
    pub fn register_roles_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RolesGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::RolesGetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::RolesGet, _, _>(handler);
    }
    pub fn register_roles_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RolesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::RolesListOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::RolesList, _, _>(handler);
    }
    pub fn register_roles_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RolesPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::RolesPutOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::RolesPut, _, _>(handler);
    }
    pub fn register_service_instances_disable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ServiceInstancesDisableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ServiceInstancesDisableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ServiceInstancesDisable, _, _>(handler);
    }
    pub fn register_service_instances_enable<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ServiceInstancesEnableInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ServiceInstancesEnableOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ServiceInstancesEnable, _, _>(handler);
    }
    pub fn register_service_instances_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ServiceInstancesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ServiceInstancesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ServiceInstancesList, _, _>(handler);
    }
    pub fn register_service_instances_provision<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::ServiceInstancesProvisionInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ServiceInstancesProvisionOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ServiceInstancesProvision, _, _>(handler);
    }
    pub fn register_service_instances_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ServiceInstancesRemoveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ServiceInstancesRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ServiceInstancesRemove, _, _>(handler);
    }
    pub fn register_sessions_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SessionsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::SessionsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::SessionsList, _, _>(handler);
    }
    pub fn register_sessions_logout<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SessionsLogoutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::SessionsLogoutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::SessionsLogout, _, _>(handler);
    }
    pub fn register_sessions_me<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SessionsMeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::SessionsMeOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::SessionsMe, _, _>(handler);
    }
    pub fn register_sessions_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SessionsRevokeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::SessionsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::SessionsRevoke, _, _>(handler);
    }
    pub fn register_user_identities_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UserIdentitiesListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::UserIdentitiesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::UserIdentitiesList, _, _>(handler);
    }
    pub fn register_user_identities_unlink<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UserIdentitiesUnlinkInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::UserIdentitiesUnlinkOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::UserIdentitiesUnlink, _, _>(handler);
    }
    pub fn register_users_create<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersCreateInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::UsersCreateOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::UsersCreate, _, _>(handler);
    }
    pub fn register_users_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::UsersGetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::UsersGet, _, _>(handler);
    }
    pub fn register_users_identity_link_create<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersIdentityLinkCreateInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::UsersIdentityLinkCreateOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::UsersIdentityLinkCreate, _, _>(handler);
    }
    pub fn register_users_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::UsersListOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::UsersList, _, _>(handler);
    }
    pub fn register_users_password_change<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersPasswordChangeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::UsersPasswordChangeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::UsersPasswordChange, _, _>(handler);
    }
    pub fn register_users_password_reset_create<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::UsersPasswordResetCreateInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::UsersPasswordResetCreateOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::UsersPasswordResetCreate, _, _>(handler);
    }
    pub fn register_users_resolve<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersResolveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::UsersResolveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::UsersResolve, _, _>(handler);
    }
    pub fn register_users_update<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::UsersUpdateInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::UsersUpdateOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::UsersUpdate, _, _>(handler);
    }
}
