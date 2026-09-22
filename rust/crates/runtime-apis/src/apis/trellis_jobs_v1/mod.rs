//! Generated API `trellis.jobs@v1`.
pub const API_ID: &str = "trellis.jobs@v1";
pub const API_DIGEST: &str = "pgIp9PftLViyn-1-PKqw2kkCsEWAJYHurAEA158ylso";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct NotFoundError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl NotFoundError {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::JobsNotFoundErrorData, serde_json::Error> {
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
    impl std::fmt::Display for NotFoundError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for NotFoundError {}
    impl trellis_rs::generated::TrellisError for NotFoundError {
        const TYPE: &'static str = "trellis.jobs@v1::NotFoundError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UnexpectedError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl std::fmt::Display for UnexpectedError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for UnexpectedError {}
    impl trellis_rs::generated::TrellisError for UnexpectedError {
        const TYPE: &'static str = "trellis.jobs@v1::UnexpectedError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct ValidationError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl std::fmt::Display for ValidationError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for ValidationError {}
    impl trellis_rs::generated::TrellisError for ValidationError {
        const TYPE: &'static str = "trellis.jobs@v1::ValidationError";
    }
}
pub mod rpc {
    pub type CancelInput = crate::__types::trellis::JobsCancelRequest;
    pub type CancelOutput = crate::__types::trellis::JobsCancelResponse;
    pub struct Cancel;
    impl Cancel {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Cancel";
        pub const KEY: &'static str = "jobs.Cancel";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.Cancel";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::NotFoundError",
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CancelError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CancelError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Cancel {
        type Input = CancelInput;
        type Output = CancelOutput;
        type Error = CancelError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CancelError::decode(value)
        }
    }
    pub type DismissDLQInput = crate::__types::trellis::JobsDismissDLQRequest;
    pub type DismissDLQOutput = crate::__types::trellis::JobsDismissDLQResponse;
    pub struct DismissDLQ;
    impl DismissDLQ {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DismissDLQ";
        pub const KEY: &'static str = "jobs.DismissDLQ";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.DismissDLQ";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::NotFoundError",
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DismissDLQError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DismissDLQError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DismissDLQ {
        type Input = DismissDLQInput;
        type Output = DismissDLQOutput;
        type Error = DismissDLQError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DismissDLQError::decode(value)
        }
    }
    pub type GetKeyInput = crate::__types::trellis::JobsGetKeyRequest;
    pub type GetKeyOutput = crate::__types::trellis::JobsGetKeyResponse;
    pub struct GetKey;
    impl GetKey {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.GetKey";
        pub const KEY: &'static str = "jobs.GetKey";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.GetKey";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::NotFoundError",
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum GetKeyError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl GetKeyError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for GetKey {
        type Input = GetKeyInput;
        type Output = GetKeyOutput;
        type Error = GetKeyError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            GetKeyError::decode(value)
        }
    }
    pub type InspectInput = crate::__types::trellis::JobsInspectRequest;
    pub type InspectOutput = crate::__types::trellis::JobsInspectResponse;
    pub struct Inspect;
    impl Inspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Inspect";
        pub const KEY: &'static str = "jobs.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::NotFoundError",
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum InspectError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl InspectError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Inspect {
        type Input = InspectInput;
        type Output = InspectOutput;
        type Error = InspectError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            InspectError::decode(value)
        }
    }
    pub type ListDLQInput = crate::__types::trellis::JobsListDLQRequest;
    pub type ListDLQOutput = crate::__types::trellis::JobsListDLQResponse;
    pub struct ListDLQ;
    impl ListDLQ {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ListDLQ";
        pub const KEY: &'static str = "jobs.ListDLQ";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.ListDLQ";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ListDLQError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ListDLQError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ListDLQ {
        type Input = ListDLQInput;
        type Output = ListDLQOutput;
        type Error = ListDLQError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ListDLQError::decode(value)
        }
    }
    pub type ListServicesInput = crate::__types::trellis::JobsListServicesRequest;
    pub type ListServicesOutput = crate::__types::trellis::JobsListServicesResponse;
    pub struct ListServices;
    impl ListServices {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ListServices";
        pub const KEY: &'static str = "jobs.ListServices";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.ListServices";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ListServicesError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ListServicesError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ListServices {
        type Input = ListServicesInput;
        type Output = ListServicesOutput;
        type Error = ListServicesError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ListServicesError::decode(value)
        }
    }
    pub type MetricsInput = crate::__types::trellis::JobsMetricsRequest;
    pub type MetricsOutput = crate::__types::trellis::JobsMetricsResponse;
    pub struct Metrics;
    impl Metrics {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Metrics";
        pub const KEY: &'static str = "jobs.Metrics";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.Metrics";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum MetricsError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl MetricsError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Metrics {
        type Input = MetricsInput;
        type Output = MetricsOutput;
        type Error = MetricsError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            MetricsError::decode(value)
        }
    }
    pub type QueryInput = crate::__types::trellis::JobsQueryRequest;
    pub type QueryOutput = crate::__types::trellis::JobsQueryResponse;
    pub struct Query;
    impl Query {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Query";
        pub const KEY: &'static str = "jobs.Query";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum QueryError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl QueryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Query {
        type Input = QueryInput;
        type Output = QueryOutput;
        type Error = QueryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            QueryError::decode(value)
        }
    }
    pub type ReplayDLQInput = crate::__types::trellis::JobsReplayDLQRequest;
    pub type ReplayDLQOutput = crate::__types::trellis::JobsReplayDLQResponse;
    pub struct ReplayDLQ;
    impl ReplayDLQ {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ReplayDLQ";
        pub const KEY: &'static str = "jobs.ReplayDLQ";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.ReplayDLQ";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::NotFoundError",
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ReplayDLQError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ReplayDLQError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ReplayDLQ {
        type Input = ReplayDLQInput;
        type Output = ReplayDLQOutput;
        type Error = ReplayDLQError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ReplayDLQError::decode(value)
        }
    }
    pub type RetryInput = crate::__types::trellis::JobsRetryRequest;
    pub type RetryOutput = crate::__types::trellis::JobsRetryResponse;
    pub struct Retry;
    impl Retry {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Retry";
        pub const KEY: &'static str = "jobs.Retry";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.Retry";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::NotFoundError",
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum RetryError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl RetryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Retry {
        type Input = RetryInput;
        type Output = RetryOutput;
        type Error = RetryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            RetryError::decode(value)
        }
    }
    pub type SummaryInput = crate::__types::trellis::JobsSummaryRequest;
    pub type SummaryOutput = crate::__types::trellis::JobsSummaryResponse;
    pub struct Summary;
    impl Summary {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Summary";
        pub const KEY: &'static str = "jobs.Summary";
        pub const SUBJECT: &'static str = "rpc.v1.jobs.Summary";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.jobs@v1::UnexpectedError",
            "trellis.jobs@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum SummaryError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl SummaryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.jobs@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.jobs@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Summary {
        type Input = SummaryInput;
        type Output = SummaryOutput;
        type Error = SummaryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            SummaryError::decode(value)
        }
    }
}
pub mod operations {}
pub mod events {}
pub mod feeds {
    pub type WatchInput = crate::__types::trellis::JobsWatchRequest;
    pub type WatchEvent = crate::__types::trellis::JobsWatchFrame;
    pub struct Watch;
    impl Watch {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "feed.Watch";
        pub const KEY: &'static str = "jobs.Watch";
        pub const SUBJECT: &'static str = "feed.v1.jobs.Watch";
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &["trellis.jobs@v1::stream"];
    }
    impl trellis_rs::generated::FeedDescriptor for Watch {
        type Input = WatchInput;
        type Event = WatchEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::Cancel>();
    router.register_rpc_metadata::<rpc::DismissDLQ>();
    router.register_rpc_metadata::<rpc::GetKey>();
    router.register_rpc_metadata::<rpc::Inspect>();
    router.register_rpc_metadata::<rpc::ListDLQ>();
    router.register_rpc_metadata::<rpc::ListServices>();
    router.register_rpc_metadata::<rpc::Metrics>();
    router.register_rpc_metadata::<rpc::Query>();
    router.register_rpc_metadata::<rpc::ReplayDLQ>();
    router.register_rpc_metadata::<rpc::Retry>();
    router.register_rpc_metadata::<rpc::Summary>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn cancel(
        &self,
        input: &rpc::CancelInput,
    ) -> Result<rpc::CancelOutput, trellis_rs::client::CallError<rpc::CancelError>> {
        self.inner.call::<rpc::Cancel>(input).await
    }
    pub async fn dismiss_dlq(
        &self,
        input: &rpc::DismissDLQInput,
    ) -> Result<rpc::DismissDLQOutput, trellis_rs::client::CallError<rpc::DismissDLQError>> {
        self.inner.call::<rpc::DismissDLQ>(input).await
    }
    pub async fn get_key(
        &self,
        input: &rpc::GetKeyInput,
    ) -> Result<rpc::GetKeyOutput, trellis_rs::client::CallError<rpc::GetKeyError>> {
        self.inner.call::<rpc::GetKey>(input).await
    }
    pub async fn inspect(
        &self,
        input: &rpc::InspectInput,
    ) -> Result<rpc::InspectOutput, trellis_rs::client::CallError<rpc::InspectError>> {
        self.inner.call::<rpc::Inspect>(input).await
    }
    pub async fn list_dlq(
        &self,
        input: &rpc::ListDLQInput,
    ) -> Result<rpc::ListDLQOutput, trellis_rs::client::CallError<rpc::ListDLQError>> {
        self.inner.call::<rpc::ListDLQ>(input).await
    }
    pub fn list_dlq_pages(
        &self,
        input: rpc::ListDLQInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ListDLQOutput, crate::PaginationError<rpc::ListDLQError>>,
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
                    .list_dlq(&input)
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
    pub fn list_dlq_items(
        &self,
        input: rpc::ListDLQInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::JobsListDLQResponseentriesItem,
            crate::PaginationError<rpc::ListDLQError>,
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
                Vec::<crate::__types::trellis::JobsListDLQResponseentriesItem>::new().into_iter(),
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
                        .list_dlq(&input)
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
    pub async fn list_services(
        &self,
        input: &rpc::ListServicesInput,
    ) -> Result<rpc::ListServicesOutput, trellis_rs::client::CallError<rpc::ListServicesError>>
    {
        self.inner.call::<rpc::ListServices>(input).await
    }
    pub fn list_services_pages(
        &self,
        input: rpc::ListServicesInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ListServicesOutput, crate::PaginationError<rpc::ListServicesError>>,
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
                    .list_services(&input)
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
    pub fn list_services_items(
        &self,
        input: rpc::ListServicesInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::JobsListServicesResponseentriesItem,
            crate::PaginationError<rpc::ListServicesError>,
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
                Vec::<crate::__types::trellis::JobsListServicesResponseentriesItem>::new()
                    .into_iter(),
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
                        .list_services(&input)
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
    pub async fn metrics(
        &self,
        input: &rpc::MetricsInput,
    ) -> Result<rpc::MetricsOutput, trellis_rs::client::CallError<rpc::MetricsError>> {
        self.inner.call::<rpc::Metrics>(input).await
    }
    pub async fn query(
        &self,
        input: &rpc::QueryInput,
    ) -> Result<rpc::QueryOutput, trellis_rs::client::CallError<rpc::QueryError>> {
        self.inner.call::<rpc::Query>(input).await
    }
    pub fn query_pages(
        &self,
        input: rpc::QueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::QueryOutput, crate::PaginationError<rpc::QueryError>>,
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
                    .query(&input)
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
    pub fn query_items(
        &self,
        input: rpc::QueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::JobsQueryResponseentriesItem,
            crate::PaginationError<rpc::QueryError>,
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
                Vec::<crate::__types::trellis::JobsQueryResponseentriesItem>::new().into_iter(),
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
                        .query(&input)
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
    pub async fn replay_dlq(
        &self,
        input: &rpc::ReplayDLQInput,
    ) -> Result<rpc::ReplayDLQOutput, trellis_rs::client::CallError<rpc::ReplayDLQError>> {
        self.inner.call::<rpc::ReplayDLQ>(input).await
    }
    pub async fn retry(
        &self,
        input: &rpc::RetryInput,
    ) -> Result<rpc::RetryOutput, trellis_rs::client::CallError<rpc::RetryError>> {
        self.inner.call::<rpc::Retry>(input).await
    }
    pub async fn summary(
        &self,
        input: &rpc::SummaryInput,
    ) -> Result<rpc::SummaryOutput, trellis_rs::client::CallError<rpc::SummaryError>> {
        self.inner.call::<rpc::Summary>(input).await
    }
    pub async fn watch(
        &self,
        input: &feeds::WatchInput,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<feeds::WatchEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner.feed::<feeds::Watch>(input).await
    }
}
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>) -> Self {
        Self { runtime }
    }
    pub fn register_cancel<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CancelInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::CancelOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Cancel, _, _>(handler);
    }
    pub fn register_dismiss_dlq<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DismissDLQInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::DismissDLQOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::DismissDLQ, _, _>(handler);
    }
    pub fn register_get_key<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::GetKeyInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::GetKeyOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::GetKey, _, _>(handler);
    }
    pub fn register_inspect<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::InspectInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::InspectOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Inspect, _, _>(handler);
    }
    pub fn register_list_dlq<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ListDLQInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::ListDLQOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::ListDLQ, _, _>(handler);
    }
    pub fn register_list_services<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ListServicesInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ListServicesOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ListServices, _, _>(handler);
    }
    pub fn register_metrics<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::MetricsInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::MetricsOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Metrics, _, _>(handler);
    }
    pub fn register_query<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::QueryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::QueryOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Query, _, _>(handler);
    }
    pub fn register_replay_dlq<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ReplayDLQInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::ReplayDLQOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::ReplayDLQ, _, _>(handler);
    }
    pub fn register_retry<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::RetryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::RetryOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Retry, _, _>(handler);
    }
    pub fn register_summary<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::SummaryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::SummaryOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Summary, _, _>(handler);
    }
    pub fn register_watch<F, S>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, feeds::WatchInput) -> S
            + Send
            + Sync
            + 'static,
        S: futures_util::Stream<Item = Result<feeds::WatchEvent, trellis_rs::service::ServerError>>
            + Send
            + 'static,
    {
        self.runtime.register_feed::<feeds::Watch, _, _>(handler);
    }
}
