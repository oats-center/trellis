//! Generated API `trellis.events@v1`.
pub const API_ID: &str = "trellis.events@v1";
pub const API_DIGEST: &str = "VnKi_htsISQvdvdTIQyNKNDwsWTeeXELZtL8AXUCrnQ";
pub struct Api;
impl trellis_rs::generated::ApiDescriptor for Api {
    const ID: &'static str = API_ID;
}
pub mod errors {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Conflict {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl Conflict {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::EventsErrorData, serde_json::Error> {
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
    impl std::fmt::Display for Conflict {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for Conflict {}
    impl trellis_rs::generated::TrellisError for Conflict {
        const TYPE: &'static str = "trellis.events@v1::Conflict";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Forbidden {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl Forbidden {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::EventsErrorData, serde_json::Error> {
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
    impl std::fmt::Display for Forbidden {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for Forbidden {}
    impl trellis_rs::generated::TrellisError for Forbidden {
        const TYPE: &'static str = "trellis.events@v1::Forbidden";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct NotFoundError {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl NotFoundError {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::EventsNotFoundErrorData, serde_json::Error> {
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
        const TYPE: &'static str = "trellis.events@v1::NotFoundError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Unavailable {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl Unavailable {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::EventsErrorData, serde_json::Error> {
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
    impl std::fmt::Display for Unavailable {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for Unavailable {}
    impl trellis_rs::generated::TrellisError for Unavailable {
        const TYPE: &'static str = "trellis.events@v1::Unavailable";
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
        const TYPE: &'static str = "trellis.events@v1::UnexpectedError";
    }
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UnreplayableOriginal {
        #[serde(flatten)]
        pub error: trellis_rs::generated::SerializableErrorData,
    }
    impl UnreplayableOriginal {
        pub fn payload(
            &self,
        ) -> Result<crate::__types::trellis::EventsErrorData, serde_json::Error> {
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
    impl std::fmt::Display for UnreplayableOriginal {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.error.message)
        }
    }
    impl std::error::Error for UnreplayableOriginal {}
    impl trellis_rs::generated::TrellisError for UnreplayableOriginal {
        const TYPE: &'static str = "trellis.events@v1::UnreplayableOriginal";
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
        const TYPE: &'static str = "trellis.events@v1::ValidationError";
    }
}
pub mod rpc {
    pub type ConsumersInspectInput = crate::__types::trellis::EventsConsumersInspectRequest;
    pub type ConsumersInspectOutput = crate::__types::trellis::EventsConsumersInspectResponse;
    pub struct ConsumersInspect;
    impl ConsumersInspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Consumers.Inspect";
        pub const KEY: &'static str = "events.Consumers.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.events.Consumers.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.events@v1::manage_consumers",
            "trellis.events@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::NotFoundError",
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ConsumersInspectError {
        NotFoundError(super::errors::NotFoundError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ConsumersInspectError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ConsumersInspect {
        type Input = ConsumersInspectInput;
        type Output = ConsumersInspectOutput;
        type Error = ConsumersInspectError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ConsumersInspectError::decode(value)
        }
    }
    pub type ConsumersQueryInput = crate::__types::trellis::EventsConsumersQueryRequest;
    pub type ConsumersQueryOutput = crate::__types::trellis::EventsConsumersQueryResponse;
    pub struct ConsumersQuery;
    impl ConsumersQuery {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Consumers.Query";
        pub const KEY: &'static str = "events.Consumers.Query";
        pub const SUBJECT: &'static str = "rpc.v1.events.Consumers.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.events@v1::manage_consumers",
            "trellis.events@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ConsumersQueryError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ConsumersQueryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ConsumersQuery {
        type Input = ConsumersQueryInput;
        type Output = ConsumersQueryOutput;
        type Error = ConsumersQueryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ConsumersQueryError::decode(value)
        }
    }
    pub type ConsumersReportDeliveryInput =
        crate::__types::trellis::EventsConsumersReportDeliveryRequest;
    pub type ConsumersReportDeliveryOutput =
        crate::__types::trellis::EventsConsumersReportDeliveryResponse;
    pub struct ConsumersReportDelivery;
    impl ConsumersReportDelivery {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Consumers.ReportDelivery";
        pub const KEY: &'static str = "events.Consumers.ReportDelivery";
        pub const SUBJECT: &'static str = "rpc.v1.events.Consumers.ReportDelivery";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.events@v1::public"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::Conflict",
            "trellis.events@v1::Forbidden",
            "trellis.events@v1::NotFoundError",
            "trellis.events@v1::Unavailable",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ConsumersReportDeliveryError {
        Conflict(super::errors::Conflict),
        Forbidden(super::errors::Forbidden),
        NotFoundError(super::errors::NotFoundError),
        Unavailable(super::errors::Unavailable),
        ValidationError(super::errors::ValidationError),
    }
    impl ConsumersReportDeliveryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::Conflict") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Conflict>(value)
                        .map(|value| value.map(Self::Conflict))
                }
                Some("trellis.events@v1::Forbidden") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Forbidden>(value)
                        .map(|value| value.map(Self::Forbidden))
                }
                Some("trellis.events@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.events@v1::Unavailable") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Unavailable>(value)
                        .map(|value| value.map(Self::Unavailable))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for ConsumersReportDelivery {
        type Input = ConsumersReportDeliveryInput;
        type Output = ConsumersReportDeliveryOutput;
        type Error = ConsumersReportDeliveryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ConsumersReportDeliveryError::decode(value)
        }
    }
    pub type DeadLettersDismissInput = crate::__types::trellis::EventsDeadLettersDismissRequest;
    pub type DeadLettersDismissOutput = crate::__types::trellis::EventsDeadLettersDismissResponse;
    pub struct DeadLettersDismiss;
    impl DeadLettersDismiss {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeadLetters.Dismiss";
        pub const KEY: &'static str = "events.DeadLetters.Dismiss";
        pub const SUBJECT: &'static str = "rpc.v1.events.DeadLetters.Dismiss";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.events@v1::manage_consumers",
            "trellis.events@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::Conflict",
            "trellis.events@v1::Forbidden",
            "trellis.events@v1::NotFoundError",
            "trellis.events@v1::Unavailable",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeadLettersDismissError {
        Conflict(super::errors::Conflict),
        Forbidden(super::errors::Forbidden),
        NotFoundError(super::errors::NotFoundError),
        Unavailable(super::errors::Unavailable),
        ValidationError(super::errors::ValidationError),
    }
    impl DeadLettersDismissError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::Conflict") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Conflict>(value)
                        .map(|value| value.map(Self::Conflict))
                }
                Some("trellis.events@v1::Forbidden") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Forbidden>(value)
                        .map(|value| value.map(Self::Forbidden))
                }
                Some("trellis.events@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.events@v1::Unavailable") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Unavailable>(value)
                        .map(|value| value.map(Self::Unavailable))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeadLettersDismiss {
        type Input = DeadLettersDismissInput;
        type Output = DeadLettersDismissOutput;
        type Error = DeadLettersDismissError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeadLettersDismissError::decode(value)
        }
    }
    pub type DeadLettersInspectInput = crate::__types::trellis::EventsDeadLettersInspectRequest;
    pub type DeadLettersInspectOutput = crate::__types::trellis::EventsDeadLettersInspectResponse;
    pub struct DeadLettersInspect;
    impl DeadLettersInspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeadLetters.Inspect";
        pub const KEY: &'static str = "events.DeadLetters.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.events.DeadLetters.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.events@v1::manage_consumers",
            "trellis.events@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::Forbidden",
            "trellis.events@v1::NotFoundError",
            "trellis.events@v1::Unavailable",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeadLettersInspectError {
        Forbidden(super::errors::Forbidden),
        NotFoundError(super::errors::NotFoundError),
        Unavailable(super::errors::Unavailable),
        ValidationError(super::errors::ValidationError),
    }
    impl DeadLettersInspectError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::Forbidden") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Forbidden>(value)
                        .map(|value| value.map(Self::Forbidden))
                }
                Some("trellis.events@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.events@v1::Unavailable") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Unavailable>(value)
                        .map(|value| value.map(Self::Unavailable))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeadLettersInspect {
        type Input = DeadLettersInspectInput;
        type Output = DeadLettersInspectOutput;
        type Error = DeadLettersInspectError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeadLettersInspectError::decode(value)
        }
    }
    pub type DeadLettersQueryInput = crate::__types::trellis::EventsDeadLettersQueryRequest;
    pub type DeadLettersQueryOutput = crate::__types::trellis::EventsDeadLettersQueryResponse;
    pub struct DeadLettersQuery;
    impl DeadLettersQuery {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeadLetters.Query";
        pub const KEY: &'static str = "events.DeadLetters.Query";
        pub const SUBJECT: &'static str = "rpc.v1.events.DeadLetters.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.events@v1::manage_consumers",
            "trellis.events@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::Forbidden",
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeadLettersQueryError {
        Forbidden(super::errors::Forbidden),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeadLettersQueryError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::Forbidden") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::Forbidden>(value)
                        .map(|value| value.map(Self::Forbidden))
                }
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeadLettersQuery {
        type Input = DeadLettersQueryInput;
        type Output = DeadLettersQueryOutput;
        type Error = DeadLettersQueryError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeadLettersQueryError::decode(value)
        }
    }
    pub type DeadLettersReplayInput = crate::__types::trellis::EventsDeadLettersReplayRequest;
    pub type DeadLettersReplayOutput = crate::__types::trellis::EventsDeadLettersReplayResponse;
    pub struct DeadLettersReplay;
    impl DeadLettersReplay {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeadLetters.Replay";
        pub const KEY: &'static str = "events.DeadLetters.Replay";
        pub const SUBJECT: &'static str = "rpc.v1.events.DeadLetters.Replay";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &[
            "trellis.events@v1::manage_consumers",
            "trellis.events@v1::public",
        ];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::Conflict",
            "trellis.events@v1::Forbidden",
            "trellis.events@v1::NotFoundError",
            "trellis.events@v1::Unavailable",
            "trellis.events@v1::UnreplayableOriginal",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeadLettersReplayError {
        Conflict(super::errors::Conflict),
        Forbidden(super::errors::Forbidden),
        NotFoundError(super::errors::NotFoundError),
        Unavailable(super::errors::Unavailable),
        UnreplayableOriginal(super::errors::UnreplayableOriginal),
        ValidationError(super::errors::ValidationError),
    }
    impl DeadLettersReplayError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::Conflict") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::Conflict,
                    >(value)
                        .map(|value| value.map(Self::Conflict))
                }
                Some("trellis.events@v1::Forbidden") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::Forbidden,
                    >(value)
                        .map(|value| value.map(Self::Forbidden))
                }
                Some("trellis.events@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::NotFoundError,
                    >(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.events@v1::Unavailable") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::Unavailable,
                    >(value)
                        .map(|value| value.map(Self::Unavailable))
                }
                Some("trellis.events@v1::UnreplayableOriginal") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::UnreplayableOriginal,
                    >(value)
                        .map(|value| value.map(Self::UnreplayableOriginal))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<
                        super::errors::ValidationError,
                    >(value)
                        .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for DeadLettersReplay {
        type Input = DeadLettersReplayInput;
        type Output = DeadLettersReplayOutput;
        type Error = DeadLettersReplayError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeadLettersReplayError::decode(value)
        }
    }
    pub type DiagnosticsInput = crate::__types::trellis::EventsDiagnosticsRequest;
    pub type DiagnosticsOutput = crate::__types::trellis::EventsDiagnosticsResponse;
    pub struct Diagnostics;
    impl Diagnostics {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Diagnostics";
        pub const KEY: &'static str = "events.Diagnostics";
        pub const SUBJECT: &'static str = "rpc.v1.events.Diagnostics";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.events@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DiagnosticsError {
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DiagnosticsError {
        pub fn decode(value: serde_json::Value) -> Result<Option<Self>, serde_json::Error> {
            let error_type = value.get("type").and_then(serde_json::Value::as_str);
            match error_type {
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::ValidationError>(
                        value,
                    )
                    .map(|value| value.map(Self::ValidationError))
                }
                _ => Ok(None),
            }
        }
    }
    impl trellis_rs::generated::RpcDescriptor for Diagnostics {
        type Input = DiagnosticsInput;
        type Output = DiagnosticsOutput;
        type Error = DiagnosticsError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DiagnosticsError::decode(value)
        }
    }
    pub type InspectInput = crate::__types::trellis::EventsInspectRequest;
    pub type InspectOutput = crate::__types::trellis::EventsInspectResponse;
    pub struct Inspect;
    impl Inspect {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Inspect";
        pub const KEY: &'static str = "events.Inspect";
        pub const SUBJECT: &'static str = "rpc.v1.events.Inspect";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.events@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::NotFoundError",
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
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
                Some("trellis.events@v1::NotFoundError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::NotFoundError>(value)
                        .map(|value| value.map(Self::NotFoundError))
                }
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
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
    pub type MetricsInput = crate::__types::trellis::EventsMetricsRequest;
    pub type MetricsOutput = crate::__types::trellis::EventsMetricsResponse;
    pub struct Metrics;
    impl Metrics {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Metrics";
        pub const KEY: &'static str = "events.Metrics";
        pub const SUBJECT: &'static str = "rpc.v1.events.Metrics";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.events@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
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
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
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
    pub type QueryInput = crate::__types::trellis::EventsQueryRequest;
    pub type QueryOutput = crate::__types::trellis::EventsQueryResponse;
    pub struct Query;
    impl Query {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Query";
        pub const KEY: &'static str = "events.Query";
        pub const SUBJECT: &'static str = "rpc.v1.events.Query";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.events@v1::read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.events@v1::UnexpectedError",
            "trellis.events@v1::ValidationError",
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
                Some("trellis.events@v1::UnexpectedError") => {
                    trellis_rs::generated::decode_typed_error::<super::errors::UnexpectedError>(
                        value,
                    )
                    .map(|value| value.map(Self::UnexpectedError))
                }
                Some("trellis.events@v1::ValidationError") => {
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
}
pub mod operations {}
pub mod events {}
pub mod lives {
    pub type WatchInput = crate::__types::trellis::EventsWatchRequest;
    pub type WatchEvent = crate::__types::trellis::EventsWatchFrame;
    pub struct Watch;
    impl Watch {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "live.Watch";
        pub const KEY: &'static str = "events.Watch";
        pub const SUBJECT: &'static str = "live.v1.events.Watch";
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &["trellis.events@v1::stream"];
    }
    impl trellis_rs::generated::LiveDescriptor for Watch {
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
    router.register_rpc_metadata::<rpc::ConsumersInspect>();
    router.register_rpc_metadata::<rpc::ConsumersQuery>();
    router.register_rpc_metadata::<rpc::ConsumersReportDelivery>();
    router.register_rpc_metadata::<rpc::DeadLettersDismiss>();
    router.register_rpc_metadata::<rpc::DeadLettersInspect>();
    router.register_rpc_metadata::<rpc::DeadLettersQuery>();
    router.register_rpc_metadata::<rpc::DeadLettersReplay>();
    router.register_rpc_metadata::<rpc::Diagnostics>();
    router.register_rpc_metadata::<rpc::Inspect>();
    router.register_rpc_metadata::<rpc::Metrics>();
    router.register_rpc_metadata::<rpc::Query>();
}
#[derive(Clone)]
pub struct Client {
    inner: trellis_rs::generated::Client,
}
impl Client {
    pub fn from_generated(inner: trellis_rs::generated::Client) -> Self {
        Self { inner }
    }
    pub async fn consumers_inspect(
        &self,
        input: &rpc::ConsumersInspectInput,
    ) -> Result<
        rpc::ConsumersInspectOutput,
        trellis_rs::client::CallError<rpc::ConsumersInspectError>,
    > {
        self.inner.call::<rpc::ConsumersInspect>(input).await
    }
    pub async fn consumers_query(
        &self,
        input: &rpc::ConsumersQueryInput,
    ) -> Result<rpc::ConsumersQueryOutput, trellis_rs::client::CallError<rpc::ConsumersQueryError>>
    {
        self.inner.call::<rpc::ConsumersQuery>(input).await
    }
    pub fn consumers_query_pages(
        &self,
        input: rpc::ConsumersQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ConsumersQueryOutput, crate::PaginationError<rpc::ConsumersQueryError>>,
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
                    .consumers_query(&input)
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
    pub fn consumers_query_items(
        &self,
        input: rpc::ConsumersQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::EventConsumerStatusRow,
            crate::PaginationError<rpc::ConsumersQueryError>,
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
                Vec::<crate::__types::trellis::EventConsumerStatusRow>::new().into_iter(),
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
                        .consumers_query(&input)
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
    pub async fn consumers_report_delivery(
        &self,
        input: &rpc::ConsumersReportDeliveryInput,
    ) -> Result<
        rpc::ConsumersReportDeliveryOutput,
        trellis_rs::client::CallError<rpc::ConsumersReportDeliveryError>,
    > {
        self.inner.call::<rpc::ConsumersReportDelivery>(input).await
    }
    pub async fn dead_letters_dismiss(
        &self,
        input: &rpc::DeadLettersDismissInput,
    ) -> Result<
        rpc::DeadLettersDismissOutput,
        trellis_rs::client::CallError<rpc::DeadLettersDismissError>,
    > {
        self.inner.call::<rpc::DeadLettersDismiss>(input).await
    }
    pub async fn dead_letters_inspect(
        &self,
        input: &rpc::DeadLettersInspectInput,
    ) -> Result<
        rpc::DeadLettersInspectOutput,
        trellis_rs::client::CallError<rpc::DeadLettersInspectError>,
    > {
        self.inner.call::<rpc::DeadLettersInspect>(input).await
    }
    pub async fn dead_letters_query(
        &self,
        input: &rpc::DeadLettersQueryInput,
    ) -> Result<
        rpc::DeadLettersQueryOutput,
        trellis_rs::client::CallError<rpc::DeadLettersQueryError>,
    > {
        self.inner.call::<rpc::DeadLettersQuery>(input).await
    }
    pub fn dead_letters_query_pages(
        &self,
        input: rpc::DeadLettersQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::DeadLettersQueryOutput, crate::PaginationError<rpc::DeadLettersQueryError>>,
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
                    .dead_letters_query(&input)
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
    pub fn dead_letters_query_items(
        &self,
        input: rpc::DeadLettersQueryInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::EventsDeadLetter,
            crate::PaginationError<rpc::DeadLettersQueryError>,
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
                Vec::<crate::__types::trellis::EventsDeadLetter>::new().into_iter(),
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
                        .dead_letters_query(&input)
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
    pub async fn dead_letters_replay(
        &self,
        input: &rpc::DeadLettersReplayInput,
    ) -> Result<
        rpc::DeadLettersReplayOutput,
        trellis_rs::client::CallError<rpc::DeadLettersReplayError>,
    > {
        self.inner.call::<rpc::DeadLettersReplay>(input).await
    }
    pub async fn diagnostics(
        &self,
        input: &rpc::DiagnosticsInput,
    ) -> Result<rpc::DiagnosticsOutput, trellis_rs::client::CallError<rpc::DiagnosticsError>> {
        self.inner.call::<rpc::Diagnostics>(input).await
    }
    pub async fn inspect(
        &self,
        input: &rpc::InspectInput,
    ) -> Result<rpc::InspectOutput, trellis_rs::client::CallError<rpc::InspectError>> {
        self.inner.call::<rpc::Inspect>(input).await
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
        Result<crate::__types::trellis::EventsRow, crate::PaginationError<rpc::QueryError>>,
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
                Vec::<crate::__types::trellis::EventsRow>::new().into_iter(),
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
    pub async fn watch(
        &self,
        input: &lives::WatchInput,
    ) -> Result<
        trellis_rs::LiveSubscription<lives::WatchEvent>,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner.live::<lives::Watch>(input).await
    }
}
pub struct Provider<'a, P> {
    runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>,
}
impl<'a, P: trellis_rs::generated::ParticipantDescriptor> Provider<'a, P> {
    pub fn new(runtime: &'a mut trellis_rs::service::ConnectedServiceRuntime<P>) -> Self {
        Self { runtime }
    }
    pub fn register_consumers_inspect<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ConsumersInspectInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ConsumersInspectOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ConsumersInspect, _, _>(handler);
    }
    pub fn register_consumers_query<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ConsumersQueryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ConsumersQueryOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ConsumersQuery, _, _>(handler);
    }
    pub fn register_consumers_report_delivery<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ConsumersReportDeliveryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ConsumersReportDeliveryOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ConsumersReportDelivery, _, _>(handler);
    }
    pub fn register_dead_letters_dismiss<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeadLettersDismissInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeadLettersDismissOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeadLettersDismiss, _, _>(handler);
    }
    pub fn register_dead_letters_inspect<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeadLettersInspectInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeadLettersInspectOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeadLettersInspect, _, _>(handler);
    }
    pub fn register_dead_letters_query<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeadLettersQueryInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeadLettersQueryOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeadLettersQuery, _, _>(handler);
    }
    pub fn register_dead_letters_replay<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DeadLettersReplayInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeadLettersReplayOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeadLettersReplay, _, _>(handler);
    }
    pub fn register_diagnostics<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::DiagnosticsInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::DiagnosticsOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::Diagnostics, _, _>(handler);
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
    pub fn register_watch<F, S>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceLiveHandlerContext, lives::WatchInput) -> S
            + Send
            + Sync
            + 'static,
        S: futures_util::Stream<Item = Result<lives::WatchEvent, trellis_rs::service::ServerError>>
            + Send
            + 'static,
    {
        self.runtime.register_live::<lives::Watch, _, _>(handler);
    }
}
