//! Generated API `trellis.auth@v1`.
pub const API_ID: &str = "trellis.auth@v1";
pub const API_DIGEST: &str = "gXd9NIBjIa49mInD88kcXFC5_pVuL7cvibchdpdXvKo";
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
    pub type CapabilitiesListInput = crate::__types::trellis::AuthCapabilitiesListRequest;
    pub type CapabilitiesListOutput = crate::__types::trellis::AuthCapabilitiesListResponse;
    pub struct CapabilitiesList;
    impl CapabilitiesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Capabilities.List";
        pub const KEY: &'static str = "auth.Capabilities.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Capabilities.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilities_read"];
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
    pub type CapabilityGroupsDeleteInput =
        crate::__types::trellis::AuthCapabilityGroupsDeleteRequest;
    pub type CapabilityGroupsDeleteOutput =
        crate::__types::trellis::AuthCapabilityGroupsDeleteResponse;
    pub struct CapabilityGroupsDelete;
    impl CapabilityGroupsDelete {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGroups.Delete";
        pub const KEY: &'static str = "auth.CapabilityGroups.Delete";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGroups.Delete";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGroupsDeleteError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGroupsDeleteError {
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
    impl trellis_rs::generated::RpcDescriptor for CapabilityGroupsDelete {
        type Input = CapabilityGroupsDeleteInput;
        type Output = CapabilityGroupsDeleteOutput;
        type Error = CapabilityGroupsDeleteError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGroupsDeleteError::decode(value)
        }
    }
    pub type CapabilityGroupsGetInput = crate::__types::trellis::AuthCapabilityGroupsGetRequest;
    pub type CapabilityGroupsGetOutput = crate::__types::trellis::AuthCapabilityGroupsGetResponse;
    pub struct CapabilityGroupsGet;
    impl CapabilityGroupsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGroups.Get";
        pub const KEY: &'static str = "auth.CapabilityGroups.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGroups.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilities_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGroupsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGroupsGetError {
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
    impl trellis_rs::generated::RpcDescriptor for CapabilityGroupsGet {
        type Input = CapabilityGroupsGetInput;
        type Output = CapabilityGroupsGetOutput;
        type Error = CapabilityGroupsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGroupsGetError::decode(value)
        }
    }
    pub type CapabilityGroupsListInput = crate::__types::trellis::AuthCapabilityGroupsListRequest;
    pub type CapabilityGroupsListOutput = crate::__types::trellis::AuthCapabilityGroupsListResponse;
    pub struct CapabilityGroupsList;
    impl CapabilityGroupsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGroups.List";
        pub const KEY: &'static str = "auth.CapabilityGroups.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGroups.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::capabilities_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGroupsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGroupsListError {
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
    impl trellis_rs::generated::RpcDescriptor for CapabilityGroupsList {
        type Input = CapabilityGroupsListInput;
        type Output = CapabilityGroupsListOutput;
        type Error = CapabilityGroupsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGroupsListError::decode(value)
        }
    }
    pub type CapabilityGroupsPutInput = crate::__types::trellis::AuthCapabilityGroupsPutRequest;
    pub type CapabilityGroupsPutOutput = crate::__types::trellis::AuthCapabilityGroupsPutResponse;
    pub struct CapabilityGroupsPut;
    impl CapabilityGroupsPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.CapabilityGroups.Put";
        pub const KEY: &'static str = "auth.CapabilityGroups.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.CapabilityGroups.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum CapabilityGroupsPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl CapabilityGroupsPutError {
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
    impl trellis_rs::generated::RpcDescriptor for CapabilityGroupsPut {
        type Input = CapabilityGroupsPutInput;
        type Output = CapabilityGroupsPutOutput;
        type Error = CapabilityGroupsPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            CapabilityGroupsPutError::decode(value)
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
            &["trellis.auth@v1::connections_kick"];
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
            &["trellis.auth@v1::connections_read"];
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
    pub type DeploymentsApplyInput = crate::__types::trellis::AuthDeploymentsApplyRequest;
    pub type DeploymentsApplyOutput = crate::__types::trellis::AuthDeploymentsApplyResponse;
    pub struct DeploymentsApply;
    impl DeploymentsApply {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Apply";
        pub const KEY: &'static str = "auth.Deployments.Apply";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Apply";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
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
    pub type DeploymentsCreateInput = crate::__types::trellis::AuthDeploymentsCreateRequest;
    pub type DeploymentsCreateOutput = crate::__types::trellis::AuthDeploymentsCreateResponse;
    pub struct DeploymentsCreate;
    impl DeploymentsCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Create";
        pub const KEY: &'static str = "auth.Deployments.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deployments_mutate"];
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
    pub type DeploymentsDisableInput = crate::__types::trellis::AuthDeploymentsDisableRequest;
    pub type DeploymentsDisableOutput = crate::__types::trellis::AuthDeploymentsDisableResponse;
    pub struct DeploymentsDisable;
    impl DeploymentsDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Disable";
        pub const KEY: &'static str = "auth.Deployments.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deployments_mutate"];
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
    pub type DeploymentsEnableInput = crate::__types::trellis::AuthDeploymentsEnableRequest;
    pub type DeploymentsEnableOutput = crate::__types::trellis::AuthDeploymentsEnableResponse;
    pub struct DeploymentsEnable;
    impl DeploymentsEnable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Enable";
        pub const KEY: &'static str = "auth.Deployments.Enable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Enable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deployments_mutate"];
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
    pub type DeploymentsGetInput = crate::__types::trellis::AuthDeploymentsGetRequest;
    pub type DeploymentsGetOutput = crate::__types::trellis::AuthDeploymentsGetResponse;
    pub struct DeploymentsGet;
    impl DeploymentsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Get";
        pub const KEY: &'static str = "auth.Deployments.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deployments_read"];
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
    pub type DeploymentsListInput = crate::__types::trellis::AuthDeploymentsListRequest;
    pub type DeploymentsListOutput = crate::__types::trellis::AuthDeploymentsListResponse;
    pub struct DeploymentsList;
    impl DeploymentsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.List";
        pub const KEY: &'static str = "auth.Deployments.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deployments_read"];
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
    pub type DeploymentsRemoveInput = crate::__types::trellis::AuthDeploymentsRemoveRequest;
    pub type DeploymentsRemoveOutput = crate::__types::trellis::AuthDeploymentsRemoveResponse;
    pub struct DeploymentsRemove;
    impl DeploymentsRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Deployments.Remove";
        pub const KEY: &'static str = "auth.Deployments.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Deployments.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::deployments_mutate"];
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
    pub type DeviceUserAuthoritiesListInput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesListRequest;
    pub type DeviceUserAuthoritiesListOutput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesListResponse;
    pub struct DeviceUserAuthoritiesList;
    impl DeviceUserAuthoritiesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeviceUserAuthorities.List";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.DeviceUserAuthorities.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::devices_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeviceUserAuthoritiesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeviceUserAuthoritiesListError {
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
    impl trellis_rs::generated::RpcDescriptor for DeviceUserAuthoritiesList {
        type Input = DeviceUserAuthoritiesListInput;
        type Output = DeviceUserAuthoritiesListOutput;
        type Error = DeviceUserAuthoritiesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeviceUserAuthoritiesListError::decode(value)
        }
    }
    pub type DeviceUserAuthoritiesReviewsDecideInput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesReviewsDecideRequest;
    pub type DeviceUserAuthoritiesReviewsDecideOutput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesReviewsDecideResponse;
    pub struct DeviceUserAuthoritiesReviewsDecide;
    impl DeviceUserAuthoritiesReviewsDecide {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeviceUserAuthorities.Reviews.Decide";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Reviews.Decide";
        pub const SUBJECT: &'static str = "rpc.v1.auth.DeviceUserAuthorities.Reviews.Decide";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_review"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeviceUserAuthoritiesReviewsDecideError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeviceUserAuthoritiesReviewsDecideError {
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
    impl trellis_rs::generated::RpcDescriptor for DeviceUserAuthoritiesReviewsDecide {
        type Input = DeviceUserAuthoritiesReviewsDecideInput;
        type Output = DeviceUserAuthoritiesReviewsDecideOutput;
        type Error = DeviceUserAuthoritiesReviewsDecideError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeviceUserAuthoritiesReviewsDecideError::decode(value)
        }
    }
    pub type DeviceUserAuthoritiesReviewsListInput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesReviewsListRequest;
    pub type DeviceUserAuthoritiesReviewsListOutput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesReviewsListResponse;
    pub struct DeviceUserAuthoritiesReviewsList;
    impl DeviceUserAuthoritiesReviewsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeviceUserAuthorities.Reviews.List";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Reviews.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.DeviceUserAuthorities.Reviews.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_review"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeviceUserAuthoritiesReviewsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeviceUserAuthoritiesReviewsListError {
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
    impl trellis_rs::generated::RpcDescriptor for DeviceUserAuthoritiesReviewsList {
        type Input = DeviceUserAuthoritiesReviewsListInput;
        type Output = DeviceUserAuthoritiesReviewsListOutput;
        type Error = DeviceUserAuthoritiesReviewsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeviceUserAuthoritiesReviewsListError::decode(value)
        }
    }
    pub type DeviceUserAuthoritiesRevokeInput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesRevokeRequest;
    pub type DeviceUserAuthoritiesRevokeOutput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesRevokeResponse;
    pub struct DeviceUserAuthoritiesRevoke;
    impl DeviceUserAuthoritiesRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.DeviceUserAuthorities.Revoke";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.DeviceUserAuthorities.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeviceUserAuthoritiesRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeviceUserAuthoritiesRevokeError {
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
    impl trellis_rs::generated::RpcDescriptor for DeviceUserAuthoritiesRevoke {
        type Input = DeviceUserAuthoritiesRevokeInput;
        type Output = DeviceUserAuthoritiesRevokeOutput;
        type Error = DeviceUserAuthoritiesRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeviceUserAuthoritiesRevokeError::decode(value)
        }
    }
    pub type DevicesDisableInput = crate::__types::trellis::AuthDevicesDisableRequest;
    pub type DevicesDisableOutput = crate::__types::trellis::AuthDevicesDisableResponse;
    pub struct DevicesDisable;
    impl DevicesDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Disable";
        pub const KEY: &'static str = "auth.Devices.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_mutate"];
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
    pub type DevicesEnableInput = crate::__types::trellis::AuthDevicesEnableRequest;
    pub type DevicesEnableOutput = crate::__types::trellis::AuthDevicesEnableResponse;
    pub struct DevicesEnable;
    impl DevicesEnable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Enable";
        pub const KEY: &'static str = "auth.Devices.Enable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Enable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_mutate"];
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
    pub type DevicesListInput = crate::__types::trellis::AuthDevicesListRequest;
    pub type DevicesListOutput = crate::__types::trellis::AuthDevicesListResponse;
    pub struct DevicesList;
    impl DevicesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.List";
        pub const KEY: &'static str = "auth.Devices.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::devices_read"];
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
    pub type DevicesProvisionInput = crate::__types::trellis::AuthDevicesProvisionRequest;
    pub type DevicesProvisionOutput = crate::__types::trellis::AuthDevicesProvisionResponse;
    pub struct DevicesProvision;
    impl DevicesProvision {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Provision";
        pub const KEY: &'static str = "auth.Devices.Provision";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Provision";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_mutate"];
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
    pub type DevicesRemoveInput = crate::__types::trellis::AuthDevicesRemoveRequest;
    pub type DevicesRemoveOutput = crate::__types::trellis::AuthDevicesRemoveResponse;
    pub struct DevicesRemove;
    impl DevicesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Devices.Remove";
        pub const KEY: &'static str = "auth.Devices.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Devices.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::devices_mutate"];
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
    pub type GrantsGetInput = crate::__types::trellis::AuthGrantsGetRequest;
    pub type GrantsGetOutput = crate::__types::trellis::AuthGrantsGetResponse;
    pub struct GrantsGet;
    impl GrantsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Grants.Get";
        pub const KEY: &'static str = "auth.Grants.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Grants.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum GrantsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl GrantsGetError {
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
    impl trellis_rs::generated::RpcDescriptor for GrantsGet {
        type Input = GrantsGetInput;
        type Output = GrantsGetOutput;
        type Error = GrantsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            GrantsGetError::decode(value)
        }
    }
    pub type GrantsListInput = crate::__types::trellis::AuthGrantsListRequest;
    pub type GrantsListOutput = crate::__types::trellis::AuthGrantsListResponse;
    pub struct GrantsList;
    impl GrantsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Grants.List";
        pub const KEY: &'static str = "auth.Grants.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Grants.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum GrantsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl GrantsListError {
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
    impl trellis_rs::generated::RpcDescriptor for GrantsList {
        type Input = GrantsListInput;
        type Output = GrantsListOutput;
        type Error = GrantsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            GrantsListError::decode(value)
        }
    }
    pub type GrantsRevokeInput = crate::__types::trellis::AuthGrantsRevokeRequest;
    pub type GrantsRevokeOutput = crate::__types::trellis::AuthGrantsMutationResponse;
    pub struct GrantsRevoke;
    impl GrantsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Grants.Revoke";
        pub const KEY: &'static str = "auth.Grants.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Grants.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum GrantsRevokeError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl GrantsRevokeError {
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
    impl trellis_rs::generated::RpcDescriptor for GrantsRevoke {
        type Input = GrantsRevokeInput;
        type Output = GrantsRevokeOutput;
        type Error = GrantsRevokeError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            GrantsRevokeError::decode(value)
        }
    }
    pub type GrantsSetInput = crate::__types::trellis::AuthGrantsSetRequest;
    pub type GrantsSetOutput = crate::__types::trellis::AuthGrantsMutationResponse;
    pub struct GrantsSet;
    impl GrantsSet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Grants.Set";
        pub const KEY: &'static str = "auth.Grants.Set";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Grants.Set";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum GrantsSetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl GrantsSetError {
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
    impl trellis_rs::generated::RpcDescriptor for GrantsSet {
        type Input = GrantsSetInput;
        type Output = GrantsSetOutput;
        type Error = GrantsSetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            GrantsSetError::decode(value)
        }
    }
    pub type IssuersRevokeInput = crate::__types::trellis::AuthIssuersRevokeRequest;
    pub type IssuersRevokeOutput = crate::__types::trellis::AuthIssuersRevokeResponse;
    pub struct IssuersRevoke;
    impl IssuersRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Issuers.Revoke";
        pub const KEY: &'static str = "auth.Issuers.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Issuers.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
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
    pub type ParticipantsGetInput = crate::__types::trellis::AuthParticipantsGetRequest;
    pub type ParticipantsGetOutput = crate::__types::trellis::AuthParticipantsGetResponse;
    pub struct ParticipantsGet;
    impl ParticipantsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Participants.Get";
        pub const KEY: &'static str = "auth.Participants.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Participants.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ParticipantsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ParticipantsGetError {
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
    impl trellis_rs::generated::RpcDescriptor for ParticipantsGet {
        type Input = ParticipantsGetInput;
        type Output = ParticipantsGetOutput;
        type Error = ParticipantsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ParticipantsGetError::decode(value)
        }
    }
    pub type ParticipantsInstallInput = crate::__types::trellis::AuthParticipantsInstallRequest;
    pub type ParticipantsInstallOutput = crate::__types::trellis::AuthParticipantsInstallResponse;
    pub struct ParticipantsInstall;
    impl ParticipantsInstall {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Participants.Install";
        pub const KEY: &'static str = "auth.Participants.Install";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Participants.Install";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ParticipantsInstallError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ParticipantsInstallError {
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
    impl trellis_rs::generated::RpcDescriptor for ParticipantsInstall {
        type Input = ParticipantsInstallInput;
        type Output = ParticipantsInstallOutput;
        type Error = ParticipantsInstallError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ParticipantsInstallError::decode(value)
        }
    }
    pub type ParticipantsListInput = crate::__types::trellis::AuthParticipantsListRequest;
    pub type ParticipantsListOutput = crate::__types::trellis::AuthParticipantsListResponse;
    pub struct ParticipantsList;
    impl ParticipantsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Participants.List";
        pub const KEY: &'static str = "auth.Participants.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Participants.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::authorities_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum ParticipantsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl ParticipantsListError {
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
    impl trellis_rs::generated::RpcDescriptor for ParticipantsList {
        type Input = ParticipantsListInput;
        type Output = ParticipantsListOutput;
        type Error = ParticipantsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            ParticipantsListError::decode(value)
        }
    }
    pub type PortalsGetInput = crate::__types::trellis::AuthPortalsGetRequest;
    pub type PortalsGetOutput = crate::__types::trellis::AuthPortalsGetResponse;
    pub struct PortalsGet;
    impl PortalsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.Get";
        pub const KEY: &'static str = "auth.Portals.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::portals_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsGetError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsGet {
        type Input = PortalsGetInput;
        type Output = PortalsGetOutput;
        type Error = PortalsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsGetError::decode(value)
        }
    }
    pub type PortalsGrantOverridesListInput =
        crate::__types::trellis::AuthPortalsGrantOverridesListRequest;
    pub type PortalsGrantOverridesListOutput =
        crate::__types::trellis::AuthPortalsGrantOverridesListResponse;
    pub struct PortalsGrantOverridesList;
    impl PortalsGrantOverridesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.GrantOverrides.List";
        pub const KEY: &'static str = "auth.Portals.GrantOverrides.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.GrantOverrides.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::portals_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsGrantOverridesListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsGrantOverridesListError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsGrantOverridesList {
        type Input = PortalsGrantOverridesListInput;
        type Output = PortalsGrantOverridesListOutput;
        type Error = PortalsGrantOverridesListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsGrantOverridesListError::decode(value)
        }
    }
    pub type PortalsGrantOverridesPutInput =
        crate::__types::trellis::AuthPortalsGrantOverridesPutRequest;
    pub type PortalsGrantOverridesPutOutput =
        crate::__types::trellis::AuthPortalsGrantOverridesPutResponse;
    pub struct PortalsGrantOverridesPut;
    impl PortalsGrantOverridesPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.GrantOverrides.Put";
        pub const KEY: &'static str = "auth.Portals.GrantOverrides.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.GrantOverrides.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsGrantOverridesPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsGrantOverridesPutError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsGrantOverridesPut {
        type Input = PortalsGrantOverridesPutInput;
        type Output = PortalsGrantOverridesPutOutput;
        type Error = PortalsGrantOverridesPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsGrantOverridesPutError::decode(value)
        }
    }
    pub type PortalsGrantOverridesRemoveInput =
        crate::__types::trellis::AuthPortalsGrantOverridesRemoveRequest;
    pub type PortalsGrantOverridesRemoveOutput =
        crate::__types::trellis::AuthPortalsGrantOverridesRemoveResponse;
    pub struct PortalsGrantOverridesRemove;
    impl PortalsGrantOverridesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.GrantOverrides.Remove";
        pub const KEY: &'static str = "auth.Portals.GrantOverrides.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.GrantOverrides.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsGrantOverridesRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsGrantOverridesRemoveError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsGrantOverridesRemove {
        type Input = PortalsGrantOverridesRemoveInput;
        type Output = PortalsGrantOverridesRemoveOutput;
        type Error = PortalsGrantOverridesRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsGrantOverridesRemoveError::decode(value)
        }
    }
    pub type PortalsListInput = crate::__types::trellis::AuthPortalsListRequest;
    pub type PortalsListOutput = crate::__types::trellis::AuthPortalsListResponse;
    pub struct PortalsList;
    impl PortalsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.List";
        pub const KEY: &'static str = "auth.Portals.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::portals_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = true;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsListError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsListError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsList {
        type Input = PortalsListInput;
        type Output = PortalsListOutput;
        type Error = PortalsListError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsListError::decode(value)
        }
    }
    pub type PortalsLoginSettingsGetInput =
        crate::__types::trellis::AuthPortalsLoginSettingsGetRequest;
    pub type PortalsLoginSettingsGetOutput =
        crate::__types::trellis::AuthPortalsLoginSettingsGetResponse;
    pub struct PortalsLoginSettingsGet;
    impl PortalsLoginSettingsGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.LoginSettings.Get";
        pub const KEY: &'static str = "auth.Portals.LoginSettings.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.LoginSettings.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::portals_read"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsLoginSettingsGetError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsLoginSettingsGetError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsLoginSettingsGet {
        type Input = PortalsLoginSettingsGetInput;
        type Output = PortalsLoginSettingsGetOutput;
        type Error = PortalsLoginSettingsGetError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsLoginSettingsGetError::decode(value)
        }
    }
    pub type PortalsLoginSettingsUpdateInput =
        crate::__types::trellis::AuthPortalsLoginSettingsUpdateRequest;
    pub type PortalsLoginSettingsUpdateOutput =
        crate::__types::trellis::AuthPortalsLoginSettingsUpdateResponse;
    pub struct PortalsLoginSettingsUpdate;
    impl PortalsLoginSettingsUpdate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.LoginSettings.Update";
        pub const KEY: &'static str = "auth.Portals.LoginSettings.Update";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.LoginSettings.Update";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsLoginSettingsUpdateError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsLoginSettingsUpdateError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsLoginSettingsUpdate {
        type Input = PortalsLoginSettingsUpdateInput;
        type Output = PortalsLoginSettingsUpdateOutput;
        type Error = PortalsLoginSettingsUpdateError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsLoginSettingsUpdateError::decode(value)
        }
    }
    pub type PortalsPutInput = crate::__types::trellis::AuthPortalsPutRequest;
    pub type PortalsPutOutput = crate::__types::trellis::AuthPortalsPutResponse;
    pub struct PortalsPut;
    impl PortalsPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.Put";
        pub const KEY: &'static str = "auth.Portals.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsPutError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsPut {
        type Input = PortalsPutInput;
        type Output = PortalsPutOutput;
        type Error = PortalsPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsPutError::decode(value)
        }
    }
    pub type PortalsRemoveInput = crate::__types::trellis::AuthPortalsRemoveRequest;
    pub type PortalsRemoveOutput = crate::__types::trellis::AuthPortalsRemoveResponse;
    pub struct PortalsRemove;
    impl PortalsRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.Remove";
        pub const KEY: &'static str = "auth.Portals.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsRemoveError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsRemove {
        type Input = PortalsRemoveInput;
        type Output = PortalsRemoveOutput;
        type Error = PortalsRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsRemoveError::decode(value)
        }
    }
    pub type PortalsRoutesPutInput = crate::__types::trellis::AuthPortalsRoutesPutRequest;
    pub type PortalsRoutesPutOutput = crate::__types::trellis::AuthPortalsRoutesPutResponse;
    pub struct PortalsRoutesPut;
    impl PortalsRoutesPut {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.Routes.Put";
        pub const KEY: &'static str = "auth.Portals.Routes.Put";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.Routes.Put";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsRoutesPutError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsRoutesPutError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsRoutesPut {
        type Input = PortalsRoutesPutInput;
        type Output = PortalsRoutesPutOutput;
        type Error = PortalsRoutesPutError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsRoutesPutError::decode(value)
        }
    }
    pub type PortalsRoutesRemoveInput = crate::__types::trellis::AuthPortalsRoutesRemoveRequest;
    pub type PortalsRoutesRemoveOutput = crate::__types::trellis::AuthPortalsRoutesRemoveResponse;
    pub struct PortalsRoutesRemove;
    impl PortalsRoutesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Portals.Routes.Remove";
        pub const KEY: &'static str = "auth.Portals.Routes.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Portals.Routes.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::portals_mutate"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const DOWNLOAD: bool = false;
        pub const CURSOR_PAGINATION: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum PortalsRoutesRemoveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl PortalsRoutesRemoveError {
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
    impl trellis_rs::generated::RpcDescriptor for PortalsRoutesRemove {
        type Input = PortalsRoutesRemoveInput;
        type Output = PortalsRoutesRemoveOutput;
        type Error = PortalsRoutesRemoveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const DOWNLOAD: bool = Self::DOWNLOAD;
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            PortalsRoutesRemoveError::decode(value)
        }
    }
    pub type ServiceInstancesDisableInput =
        crate::__types::trellis::AuthServiceInstancesDisableRequest;
    pub type ServiceInstancesDisableOutput =
        crate::__types::trellis::AuthServiceInstancesDisableResponse;
    pub struct ServiceInstancesDisable;
    impl ServiceInstancesDisable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Disable";
        pub const KEY: &'static str = "auth.ServiceInstances.Disable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Disable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::services_mutate"];
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
    pub type ServiceInstancesEnableInput =
        crate::__types::trellis::AuthServiceInstancesEnableRequest;
    pub type ServiceInstancesEnableOutput =
        crate::__types::trellis::AuthServiceInstancesEnableResponse;
    pub struct ServiceInstancesEnable;
    impl ServiceInstancesEnable {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Enable";
        pub const KEY: &'static str = "auth.ServiceInstances.Enable";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Enable";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::services_mutate"];
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
    pub type ServiceInstancesListInput = crate::__types::trellis::AuthServiceInstancesListRequest;
    pub type ServiceInstancesListOutput = crate::__types::trellis::AuthServiceInstancesListResponse;
    pub struct ServiceInstancesList;
    impl ServiceInstancesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.List";
        pub const KEY: &'static str = "auth.ServiceInstances.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::services_read"];
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
    pub type ServiceInstancesProvisionInput =
        crate::__types::trellis::AuthServiceInstancesProvisionRequest;
    pub type ServiceInstancesProvisionOutput =
        crate::__types::trellis::AuthServiceInstancesProvisionResponse;
    pub struct ServiceInstancesProvision;
    impl ServiceInstancesProvision {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Provision";
        pub const KEY: &'static str = "auth.ServiceInstances.Provision";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Provision";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::services_mutate"];
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
    pub type ServiceInstancesRemoveInput =
        crate::__types::trellis::AuthServiceInstancesRemoveRequest;
    pub type ServiceInstancesRemoveOutput =
        crate::__types::trellis::AuthServiceInstancesRemoveResponse;
    pub struct ServiceInstancesRemove;
    impl ServiceInstancesRemove {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.ServiceInstances.Remove";
        pub const KEY: &'static str = "auth.ServiceInstances.Remove";
        pub const SUBJECT: &'static str = "rpc.v1.auth.ServiceInstances.Remove";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::services_mutate"];
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
    pub type SessionsListInput = crate::__types::trellis::AuthSessionsListRequest;
    pub type SessionsListOutput = crate::__types::trellis::AuthSessionsListResponse;
    pub struct SessionsList;
    impl SessionsList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.List";
        pub const KEY: &'static str = "auth.Sessions.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::sessions_read"];
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
    pub type SessionsLogoutInput = crate::__types::trellis::AuthSessionsLogoutRequest;
    pub type SessionsLogoutOutput = crate::__types::trellis::AuthSessionsLogoutResponse;
    pub struct SessionsLogout;
    impl SessionsLogout {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.Logout";
        pub const KEY: &'static str = "auth.Sessions.Logout";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.Logout";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
    pub type SessionsMeInput = crate::__types::trellis::AuthSessionsMeRequest;
    pub type SessionsMeOutput = crate::__types::trellis::AuthSessionsMeResponse;
    pub struct SessionsMe;
    impl SessionsMe {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.Me";
        pub const KEY: &'static str = "auth.Sessions.Me";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.Me";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
    pub type SessionsRevokeInput = crate::__types::trellis::AuthSessionsRevokeRequest;
    pub type SessionsRevokeOutput = crate::__types::trellis::AuthSessionsRevokeResponse;
    pub struct SessionsRevoke;
    impl SessionsRevoke {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Sessions.Revoke";
        pub const KEY: &'static str = "auth.Sessions.Revoke";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Sessions.Revoke";
        pub const CALLER_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::sessions_revoke"];
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
    pub type UserIdentitiesListInput = crate::__types::trellis::AuthUserIdentitiesListRequest;
    pub type UserIdentitiesListOutput = crate::__types::trellis::AuthUserIdentitiesListResponse;
    pub struct UserIdentitiesList;
    impl UserIdentitiesList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.UserIdentities.List";
        pub const KEY: &'static str = "auth.UserIdentities.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.UserIdentities.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
    pub type UserIdentitiesUnlinkInput = crate::__types::trellis::AuthUserIdentitiesUnlinkRequest;
    pub type UserIdentitiesUnlinkOutput = crate::__types::trellis::AuthUserIdentitiesUnlinkResponse;
    pub struct UserIdentitiesUnlink;
    impl UserIdentitiesUnlink {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.UserIdentities.Unlink";
        pub const KEY: &'static str = "auth.UserIdentities.Unlink";
        pub const SUBJECT: &'static str = "rpc.v1.auth.UserIdentities.Unlink";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
    pub type UsersCreateInput = crate::__types::trellis::AuthUsersCreateRequest;
    pub type UsersCreateOutput = crate::__types::trellis::AuthUsersCreateResponse;
    pub struct UsersCreate;
    impl UsersCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Create";
        pub const KEY: &'static str = "auth.Users.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::users_mutate"];
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
    pub type UsersGetInput = crate::__types::trellis::AuthUsersGetRequest;
    pub type UsersGetOutput = crate::__types::trellis::AuthUsersGetResponse;
    pub struct UsersGet;
    impl UsersGet {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Get";
        pub const KEY: &'static str = "auth.Users.Get";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Get";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::users_read"];
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
    pub type UsersIdentityLinkCreateInput =
        crate::__types::trellis::AuthUsersIdentityLinkCreateRequest;
    pub type UsersIdentityLinkCreateOutput =
        crate::__types::trellis::AuthUsersIdentityLinkCreateResponse;
    pub struct UsersIdentityLinkCreate;
    impl UsersIdentityLinkCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.IdentityLink.Create";
        pub const KEY: &'static str = "auth.Users.IdentityLink.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.IdentityLink.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
    pub type UsersListInput = crate::__types::trellis::AuthUsersListRequest;
    pub type UsersListOutput = crate::__types::trellis::AuthUsersListResponse;
    pub struct UsersList;
    impl UsersList {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.List";
        pub const KEY: &'static str = "auth.Users.List";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.List";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::users_read"];
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
    pub type UsersPasswordChangeInput = crate::__types::trellis::AuthUsersPasswordChangeRequest;
    pub type UsersPasswordChangeOutput = crate::__types::trellis::AuthUsersPasswordChangeResponse;
    pub struct UsersPasswordChange;
    impl UsersPasswordChange {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Password.Change";
        pub const KEY: &'static str = "auth.Users.Password.Change";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Password.Change";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
        crate::__types::trellis::AuthUsersPasswordResetCreateRequest;
    pub type UsersPasswordResetCreateOutput =
        crate::__types::trellis::AuthUsersPasswordResetCreateResponse;
    pub struct UsersPasswordResetCreate;
    impl UsersPasswordResetCreate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.PasswordReset.Create";
        pub const KEY: &'static str = "auth.Users.PasswordReset.Create";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.PasswordReset.Create";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::users_mutate"];
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
    pub type UsersResolveInput = crate::__types::trellis::AuthUsersResolveRequest;
    pub type UsersResolveOutput = crate::__types::trellis::AuthUsersResolveResponse;
    pub struct UsersResolve;
    impl UsersResolve {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Resolve";
        pub const KEY: &'static str = "auth.Users.Resolve";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Resolve";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::users_read"];
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
    pub type UsersUpdateInput = crate::__types::trellis::AuthUsersUpdateRequest;
    pub type UsersUpdateOutput = crate::__types::trellis::AuthUsersUpdateResponse;
    pub struct UsersUpdate;
    impl UsersUpdate {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "rpc.Users.Update";
        pub const KEY: &'static str = "auth.Users.Update";
        pub const SUBJECT: &'static str = "rpc.v1.auth.Users.Update";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::users_mutate"];
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
pub mod operations {
    pub type DeviceUserAuthoritiesResolveInput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesResolveRequest;
    pub type DeviceUserAuthoritiesResolveOutput =
        crate::__types::trellis::AuthDeviceUserAuthoritiesResolveResponse;
    pub type DeviceUserAuthoritiesResolveProgress =
        crate::__types::trellis::AuthDeviceUserAuthoritiesResolveProgress;
    pub struct DeviceUserAuthoritiesResolve;
    impl DeviceUserAuthoritiesResolve {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "operation.DeviceUserAuthorities.Resolve";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Resolve";
        pub const SUBJECT: &'static str = "operations.v1.auth.DeviceUserAuthorities.Resolve";
        pub const CALLER_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        pub const UPLOAD: bool = false;
    }
    #[derive(Clone, Debug, PartialEq)]
    pub enum DeviceUserAuthoritiesResolveError {
        AuthError(super::errors::AuthError),
        UnexpectedError(super::errors::UnexpectedError),
        ValidationError(super::errors::ValidationError),
    }
    impl DeviceUserAuthoritiesResolveError {
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
    impl trellis_rs::generated::OperationDeclaredError for DeviceUserAuthoritiesResolveError {
        fn data(&self) -> &trellis_rs::generated::SerializableErrorData {
            match self {
                Self::AuthError(value) => &value.error,
                Self::UnexpectedError(value) => &value.error,
                Self::ValidationError(value) => &value.error,
            }
        }
    }
    impl DeviceUserAuthoritiesResolveError {
        pub fn into_provider_failure(
            self,
        ) -> trellis_rs::generated::DeclaredOperationFailure<Self> {
            trellis_rs::generated::DeclaredOperationFailure::new(self)
        }
    }
    impl trellis_rs::generated::OperationDescriptor for DeviceUserAuthoritiesResolve {
        type Input = DeviceUserAuthoritiesResolveInput;
        type Output = DeviceUserAuthoritiesResolveOutput;
        type Progress = DeviceUserAuthoritiesResolveProgress;
        type Update = DeviceUserAuthoritiesResolveProgress;
        type UpdateEvidence = trellis_rs::client::DeclaredOperationUpdates;
        type Error = DeviceUserAuthoritiesResolveError;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const CALLER_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
        const ERRORS: &'static [&'static str] = &[
            "trellis.auth@v1::AuthError",
            "trellis.auth@v1::UnexpectedError",
            "trellis.auth@v1::ValidationError",
        ];
        const SIGNALS: &'static [&'static str] = &[];
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
        const UPLOAD: bool = Self::UPLOAD;
        const HAS_PROGRESS: bool = true;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some(
            "{\"$defs\":{\"trellis.AuthDeviceUserAuthoritiesResolveProgress\":{\"additionalProperties\":true,\"properties\":{\"companionConsent\":{\"$ref\":\"#/$defs/trellis.ConsentRequest\"},\"retryAfterMs\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"18446744073709551615\",\"x-trellis-minimum\":\"0\"},\"state\":{\"$ref\":\"#/$defs/trellis.AuthDeviceUserAuthoritiesResolveProgressState\"}},\"required\":[\"retryAfterMs\",\"state\"],\"type\":\"object\"},\"trellis.AuthDeviceUserAuthoritiesResolveProgressState\":{\"type\":\"string\",\"x-trellis-symbols\":[\"delegation_pending\",\"review_pending\",\"waiting\"]},\"trellis.ConsentCapability\":{\"additionalProperties\":true,\"properties\":{\"alreadyApproved\":{\"type\":\"boolean\"},\"consentDigest\":{\"type\":\"string\"},\"consequence\":{\"type\":\"string\"},\"description\":{\"type\":\"string\"},\"eligible\":{\"type\":\"boolean\"},\"id\":{\"type\":\"string\"},\"required\":{\"type\":\"boolean\"},\"title\":{\"type\":\"string\"}},\"required\":[\"alreadyApproved\",\"consentDigest\",\"consequence\",\"description\",\"eligible\",\"id\",\"required\",\"title\"],\"type\":\"object\"},\"trellis.ConsentCompanion\":{\"additionalProperties\":true,\"properties\":{\"capabilities\":{\"items\":{\"$ref\":\"#/$defs/trellis.ConsentCapability\"},\"type\":\"array\"},\"kind\":{\"$ref\":\"#/$defs/trellis.ResourceOwnerKind\"},\"participantId\":{\"type\":\"string\"},\"required\":{\"type\":\"boolean\"},\"resources\":{\"items\":{\"$ref\":\"#/$defs/trellis.ConsentResource\"},\"type\":\"array\"}},\"required\":[\"capabilities\",\"kind\",\"participantId\",\"required\",\"resources\"],\"type\":\"object\"},\"trellis.ConsentRequest\":{\"additionalProperties\":true,\"properties\":{\"capabilities\":{\"items\":{\"$ref\":\"#/$defs/trellis.ConsentCapability\"},\"type\":\"array\"},\"companion\":{\"$ref\":\"#/$defs/trellis.ConsentCompanion\"},\"decisionDigest\":{\"type\":\"string\"},\"expectedGrantRevision\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"18446744073709551615\",\"x-trellis-minimum\":\"0\"},\"installedRevision\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"18446744073709551615\",\"x-trellis-minimum\":\"0\"},\"packageDigest\":{\"type\":\"string\"},\"participantId\":{\"type\":\"string\"},\"resources\":{\"items\":{\"$ref\":\"#/$defs/trellis.ConsentResource\"},\"type\":\"array\"}},\"required\":[\"capabilities\",\"decisionDigest\",\"expectedGrantRevision\",\"installedRevision\",\"packageDigest\",\"participantId\",\"resources\"],\"type\":\"object\"},\"trellis.ConsentResource\":{\"additionalProperties\":true,\"properties\":{\"actual\":{\"$ref\":\"#/$defs/trellis.ResourceActual\"},\"alreadyApproved\":{\"type\":\"boolean\"},\"change\":{\"$ref\":\"#/$defs/trellis.ConsentResourceChange\"},\"description\":{\"type\":\"string\"},\"eligible\":{\"type\":\"boolean\"},\"kind\":{\"$ref\":\"#/$defs/trellis.ResourceKind\"},\"name\":{\"type\":\"string\"},\"requestedCommitment\":{\"$ref\":\"#/$defs/trellis.ResourceCommitment\"},\"required\":{\"type\":\"boolean\"},\"title\":{\"type\":\"string\"}},\"required\":[\"alreadyApproved\",\"change\",\"description\",\"eligible\",\"kind\",\"name\",\"requestedCommitment\",\"required\",\"title\"],\"type\":\"object\"},\"trellis.ConsentResourceChange\":{\"type\":\"string\",\"x-trellis-symbols\":[\"detached\",\"expanded\",\"incompatible\",\"new\",\"reduced\",\"unchanged\"]},\"trellis.ResourceActual\":{\"additionalProperties\":true,\"properties\":{\"history\":{\"$ref\":\"#/$defs/trellis.ResourceHistory\"},\"maxObjectBytes\":{\"$ref\":\"#/$defs/trellis.ResourceCapacityBytes\"},\"maxTotalBytes\":{\"$ref\":\"#/$defs/trellis.ResourceCapacityBytes\"},\"maxValueBytes\":{\"$ref\":\"#/$defs/trellis.ResourceCapacityBytes\"},\"representationVersion\":{\"$ref\":\"#/$defs/trellis.StateRepresentationVersion\"},\"ttlMs\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"18446744073709551615\",\"x-trellis-minimum\":\"0\"}},\"required\":[],\"type\":\"object\"},\"trellis.ResourceCapacityBytes\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"9007199254740991\",\"x-trellis-minimum\":\"0\"},\"trellis.ResourceCommitment\":{\"additionalProperties\":true,\"properties\":{\"desiredMaxObjectBytes\":{\"$ref\":\"#/$defs/trellis.ResourceCapacityBytes\"},\"desiredMaxTotalBytes\":{\"$ref\":\"#/$defs/trellis.ResourceCapacityBytes\"},\"desiredMaxValueBytes\":{\"$ref\":\"#/$defs/trellis.ResourceCapacityBytes\"},\"history\":{\"$ref\":\"#/$defs/trellis.ResourceHistory\"},\"ttlMs\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"18446744073709551615\",\"x-trellis-minimum\":\"0\"}},\"required\":[],\"type\":\"object\"},\"trellis.ResourceHistory\":{\"pattern\":\"^(0|[1-9][0-9]*)$\",\"type\":\"string\",\"x-trellis-integer\":\"uint64\",\"x-trellis-maximum\":\"9007199254740991\",\"x-trellis-minimum\":\"0\"},\"trellis.ResourceKind\":{\"type\":\"string\",\"x-trellis-symbols\":[\"consumer\",\"job\",\"kv\",\"state\",\"store\"]},\"trellis.ResourceOwnerKind\":{\"type\":\"string\",\"x-trellis-symbols\":[\"agent\",\"app\",\"device\",\"service\"]},\"trellis.StateRepresentationVersion\":{\"maximum\":4294967295,\"minimum\":1,\"type\":\"integer\"}},\"$ref\":\"#/$defs/trellis.AuthDeviceUserAuthoritiesResolveProgress\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"}",
        );
        fn decode_error(
            value: serde_json::Value,
        ) -> Result<Option<Self::Error>, serde_json::Error> {
            DeviceUserAuthoritiesResolveError::decode(value)
        }
    }
}
pub mod events {
    pub type ConnectionsClosedEvent = crate::__types::trellis::AuthConnectionsClosedEvent;
    pub struct ConnectionsClosed;
    impl ConnectionsClosed {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Connections.Closed";
        pub const KEY: &'static str = "auth.Connections.Closed";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Closed";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Closed";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
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
    pub type ConnectionsKickedEvent = crate::__types::trellis::AuthConnectionsKickedEvent;
    pub struct ConnectionsKicked;
    impl ConnectionsKicked {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Connections.Kicked";
        pub const KEY: &'static str = "auth.Connections.Kicked";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Kicked";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Kicked";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
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
    pub type ConnectionsOpenedEvent = crate::__types::trellis::AuthConnectionsOpenedEvent;
    pub struct ConnectionsOpened;
    impl ConnectionsOpened {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Connections.Opened";
        pub const KEY: &'static str = "auth.Connections.Opened";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Opened";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Connections.Opened";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
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
    pub type DeviceUserAuthoritiesApprovedEvent =
        crate::__types::trellis::AuthDeviceUserAuthoritiesApprovedEvent;
    pub struct DeviceUserAuthoritiesApproved;
    impl DeviceUserAuthoritiesApproved {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.DeviceUserAuthorities.Approved";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Approved";
        pub const SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.Approved.{/deploymentId}";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.Approved.*";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
    }
    impl trellis_rs::generated::EventDescriptor for DeviceUserAuthoritiesApproved {
        type Event = DeviceUserAuthoritiesApprovedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type DeviceUserAuthoritiesRequestedEvent =
        crate::__types::trellis::AuthDeviceUserAuthoritiesRequestedEvent;
    pub struct DeviceUserAuthoritiesRequested;
    impl DeviceUserAuthoritiesRequested {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.DeviceUserAuthorities.Requested";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Requested";
        pub const SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.Requested.{/deploymentId}";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.Requested.*";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
    }
    impl trellis_rs::generated::EventDescriptor for DeviceUserAuthoritiesRequested {
        type Event = DeviceUserAuthoritiesRequestedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type DeviceUserAuthoritiesResolvedEvent =
        crate::__types::trellis::AuthDeviceUserAuthoritiesResolvedEvent;
    pub struct DeviceUserAuthoritiesResolved;
    impl DeviceUserAuthoritiesResolved {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.DeviceUserAuthorities.Resolved";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.Resolved";
        pub const SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.Resolved.{/deploymentId}";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.Resolved.*";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
    }
    impl trellis_rs::generated::EventDescriptor for DeviceUserAuthoritiesResolved {
        type Event = DeviceUserAuthoritiesResolvedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type DeviceUserAuthoritiesReviewRequestedEvent =
        crate::__types::trellis::AuthDeviceUserAuthoritiesReviewRequestedEvent;
    pub struct DeviceUserAuthoritiesReviewRequested;
    impl DeviceUserAuthoritiesReviewRequested {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.DeviceUserAuthorities.ReviewRequested";
        pub const KEY: &'static str = "auth.DeviceUserAuthorities.ReviewRequested";
        pub const SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.ReviewRequested.{/deploymentId}";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.DeviceUserAuthorities.ReviewRequested.*";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
    }
    impl trellis_rs::generated::EventDescriptor for DeviceUserAuthoritiesReviewRequested {
        type Event = DeviceUserAuthoritiesReviewRequestedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type GrantsChangedEvent = crate::__types::trellis::AuthGrantsChangedEvent;
    pub struct GrantsChanged;
    impl GrantsChanged {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Grants.Changed";
        pub const KEY: &'static str = "auth.Grants.Changed";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Grants.Changed";
        pub const SUBSCRIBE_SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Grants.Changed";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
    }
    impl trellis_rs::generated::EventDescriptor for GrantsChanged {
        type Event = GrantsChangedEvent;
        const API_ID: &'static str = super::API_ID;
        const DESCRIPTOR_NAME: &'static str = Self::DESCRIPTOR_NAME;
        const SUBJECT: &'static str = Self::SUBJECT;
        const KEY: &'static str = Self::KEY;
        const SUBSCRIBE_SUBJECT: &'static str = Self::SUBSCRIBE_SUBJECT;
        const PUBLISH_CAPABILITIES: &'static [&'static str] = Self::PUBLISH_CAPABILITIES;
        const DELEGATED_PUBLISH: bool = Self::DELEGATED_PUBLISH;
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = Self::SUBSCRIBE_CAPABILITIES;
    }
    pub type IssuersRevokedEvent = crate::__types::trellis::AuthIssuersRevokedEvent;
    pub struct IssuersRevoked;
    impl IssuersRevoked {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Issuers.Revoked";
        pub const KEY: &'static str = "auth.Issuers.Revoked";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Issuers.Revoked";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Issuers.Revoked";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
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
    pub type SessionsRevokedEvent = crate::__types::trellis::AuthSessionsRevokedEvent;
    pub struct SessionsRevoked;
    impl SessionsRevoked {
        pub const API_ID: &'static str = super::API_ID;
        pub const DESCRIPTOR_NAME: &'static str = "event.Sessions.Revoked";
        pub const KEY: &'static str = "auth.Sessions.Revoked";
        pub const SUBJECT: &'static str = "events.v1.dHJlbGxpcy5hdXRoQHYx.Sessions.Revoked";
        pub const SUBSCRIBE_SUBJECT: &'static str =
            "events.v1.dHJlbGxpcy5hdXRoQHYx.Sessions.Revoked";
        pub const PUBLISH_CAPABILITIES: &'static [&'static str] = &["trellis.auth@v1::public"];
        pub const DELEGATED_PUBLISH: bool = true;
        pub const SUBSCRIBE_CAPABILITIES: &'static [&'static str] =
            &["trellis.auth@v1::events_stream"];
    }
    impl trellis_rs::generated::EventDescriptor for SessionsRevoked {
        type Event = SessionsRevokedEvent;
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
pub mod feeds {}
/// Registers metadata for every RPC in this API.
pub fn register_rpc_metadata(router: &mut trellis_rs::service::Router) {
    let _ = router;
    router.register_rpc_metadata::<rpc::CapabilitiesList>();
    router.register_rpc_metadata::<rpc::CapabilityGroupsDelete>();
    router.register_rpc_metadata::<rpc::CapabilityGroupsGet>();
    router.register_rpc_metadata::<rpc::CapabilityGroupsList>();
    router.register_rpc_metadata::<rpc::CapabilityGroupsPut>();
    router.register_rpc_metadata::<rpc::ConnectionsKick>();
    router.register_rpc_metadata::<rpc::ConnectionsList>();
    router.register_rpc_metadata::<rpc::DeploymentsApply>();
    router.register_rpc_metadata::<rpc::DeploymentsCreate>();
    router.register_rpc_metadata::<rpc::DeploymentsDisable>();
    router.register_rpc_metadata::<rpc::DeploymentsEnable>();
    router.register_rpc_metadata::<rpc::DeploymentsGet>();
    router.register_rpc_metadata::<rpc::DeploymentsList>();
    router.register_rpc_metadata::<rpc::DeploymentsRemove>();
    router.register_rpc_metadata::<rpc::DeviceUserAuthoritiesList>();
    router.register_rpc_metadata::<rpc::DeviceUserAuthoritiesReviewsDecide>();
    router.register_rpc_metadata::<rpc::DeviceUserAuthoritiesReviewsList>();
    router.register_rpc_metadata::<rpc::DeviceUserAuthoritiesRevoke>();
    router.register_rpc_metadata::<rpc::DevicesDisable>();
    router.register_rpc_metadata::<rpc::DevicesEnable>();
    router.register_rpc_metadata::<rpc::DevicesList>();
    router.register_rpc_metadata::<rpc::DevicesProvision>();
    router.register_rpc_metadata::<rpc::DevicesRemove>();
    router.register_rpc_metadata::<rpc::GrantsGet>();
    router.register_rpc_metadata::<rpc::GrantsList>();
    router.register_rpc_metadata::<rpc::GrantsRevoke>();
    router.register_rpc_metadata::<rpc::GrantsSet>();
    router.register_rpc_metadata::<rpc::IssuersRevoke>();
    router.register_rpc_metadata::<rpc::ParticipantsGet>();
    router.register_rpc_metadata::<rpc::ParticipantsInstall>();
    router.register_rpc_metadata::<rpc::ParticipantsList>();
    router.register_rpc_metadata::<rpc::PortalsGet>();
    router.register_rpc_metadata::<rpc::PortalsGrantOverridesList>();
    router.register_rpc_metadata::<rpc::PortalsGrantOverridesPut>();
    router.register_rpc_metadata::<rpc::PortalsGrantOverridesRemove>();
    router.register_rpc_metadata::<rpc::PortalsList>();
    router.register_rpc_metadata::<rpc::PortalsLoginSettingsGet>();
    router.register_rpc_metadata::<rpc::PortalsLoginSettingsUpdate>();
    router.register_rpc_metadata::<rpc::PortalsPut>();
    router.register_rpc_metadata::<rpc::PortalsRemove>();
    router.register_rpc_metadata::<rpc::PortalsRoutesPut>();
    router.register_rpc_metadata::<rpc::PortalsRoutesRemove>();
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
            crate::__types::trellis::AuthCapabilitiesListResponseentriesItem,
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
                Vec::<crate::__types::trellis::AuthCapabilitiesListResponseentriesItem>::new()
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
    pub async fn capability_groups_delete(
        &self,
        input: &rpc::CapabilityGroupsDeleteInput,
    ) -> Result<
        rpc::CapabilityGroupsDeleteOutput,
        trellis_rs::client::CallError<rpc::CapabilityGroupsDeleteError>,
    > {
        self.inner.call::<rpc::CapabilityGroupsDelete>(input).await
    }
    pub async fn capability_groups_get(
        &self,
        input: &rpc::CapabilityGroupsGetInput,
    ) -> Result<
        rpc::CapabilityGroupsGetOutput,
        trellis_rs::client::CallError<rpc::CapabilityGroupsGetError>,
    > {
        self.inner.call::<rpc::CapabilityGroupsGet>(input).await
    }
    pub async fn capability_groups_list(
        &self,
        input: &rpc::CapabilityGroupsListInput,
    ) -> Result<
        rpc::CapabilityGroupsListOutput,
        trellis_rs::client::CallError<rpc::CapabilityGroupsListError>,
    > {
        self.inner.call::<rpc::CapabilityGroupsList>(input).await
    }
    pub fn capability_groups_list_pages(
        &self,
        input: rpc::CapabilityGroupsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::CapabilityGroupsListOutput,
            crate::PaginationError<rpc::CapabilityGroupsListError>,
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
                    .capability_groups_list(&input)
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
    pub fn capability_groups_list_items(
        &self,
        input: rpc::CapabilityGroupsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthCapabilityGroupsListResponseentriesItem,
            crate::PaginationError<rpc::CapabilityGroupsListError>,
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
                Vec::<crate::__types::trellis::AuthCapabilityGroupsListResponseentriesItem>::new()
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
                        .capability_groups_list(&input)
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
    pub async fn capability_groups_put(
        &self,
        input: &rpc::CapabilityGroupsPutInput,
    ) -> Result<
        rpc::CapabilityGroupsPutOutput,
        trellis_rs::client::CallError<rpc::CapabilityGroupsPutError>,
    > {
        self.inner.call::<rpc::CapabilityGroupsPut>(input).await
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
            crate::__types::trellis::AuthConnectionsListResponseentriesItem,
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
                Vec::<crate::__types::trellis::AuthConnectionsListResponseentriesItem>::new()
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
            crate::__types::trellis::AuthDeploymentsListResponseentriesItem,
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
                Vec::<crate::__types::trellis::AuthDeploymentsListResponseentriesItem>::new()
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
    pub async fn device_user_authorities_list(
        &self,
        input: &rpc::DeviceUserAuthoritiesListInput,
    ) -> Result<
        rpc::DeviceUserAuthoritiesListOutput,
        trellis_rs::client::CallError<rpc::DeviceUserAuthoritiesListError>,
    > {
        self.inner
            .call::<rpc::DeviceUserAuthoritiesList>(input)
            .await
    }
    pub fn device_user_authorities_list_pages(
        &self,
        input: rpc::DeviceUserAuthoritiesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::DeviceUserAuthoritiesListOutput,
            crate::PaginationError<rpc::DeviceUserAuthoritiesListError>,
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
                    .device_user_authorities_list(&input)
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
    pub fn device_user_authorities_list_items(
        &self,
        input: rpc::DeviceUserAuthoritiesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthDeviceUserAuthoritiesListResponseentriesItem,
            crate::PaginationError<rpc::DeviceUserAuthoritiesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(
            futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<
                        crate::__types::trellis::AuthDeviceUserAuthoritiesListResponseentriesItem,
                    >::new()
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
                            .device_user_authorities_list(&input)
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
            ),
        )
    }
    pub async fn device_user_authorities_reviews_decide(
        &self,
        input: &rpc::DeviceUserAuthoritiesReviewsDecideInput,
    ) -> Result<
        rpc::DeviceUserAuthoritiesReviewsDecideOutput,
        trellis_rs::client::CallError<rpc::DeviceUserAuthoritiesReviewsDecideError>,
    > {
        self.inner
            .call::<rpc::DeviceUserAuthoritiesReviewsDecide>(input)
            .await
    }
    pub async fn device_user_authorities_reviews_list(
        &self,
        input: &rpc::DeviceUserAuthoritiesReviewsListInput,
    ) -> Result<
        rpc::DeviceUserAuthoritiesReviewsListOutput,
        trellis_rs::client::CallError<rpc::DeviceUserAuthoritiesReviewsListError>,
    > {
        self.inner
            .call::<rpc::DeviceUserAuthoritiesReviewsList>(input)
            .await
    }
    pub fn device_user_authorities_reviews_list_pages(
        &self,
        input: rpc::DeviceUserAuthoritiesReviewsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::DeviceUserAuthoritiesReviewsListOutput,
            crate::PaginationError<rpc::DeviceUserAuthoritiesReviewsListError>,
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
                    .device_user_authorities_reviews_list(&input)
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
    pub fn device_user_authorities_reviews_list_items(
        &self,
        input: rpc::DeviceUserAuthoritiesReviewsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthDeviceUserAuthoritiesReviewsListResponseentriesItem,
            crate::PaginationError<rpc::DeviceUserAuthoritiesReviewsListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(
            futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<
                        crate::__types::trellis::AuthDeviceUserAuthoritiesReviewsListResponseentriesItem,
                    >::new()
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
                            .device_user_authorities_reviews_list(&input)
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
            ),
        )
    }
    pub async fn device_user_authorities_revoke(
        &self,
        input: &rpc::DeviceUserAuthoritiesRevokeInput,
    ) -> Result<
        rpc::DeviceUserAuthoritiesRevokeOutput,
        trellis_rs::client::CallError<rpc::DeviceUserAuthoritiesRevokeError>,
    > {
        self.inner
            .call::<rpc::DeviceUserAuthoritiesRevoke>(input)
            .await
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
            crate::__types::trellis::AuthDevicesListResponseentriesItem,
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
                Vec::<crate::__types::trellis::AuthDevicesListResponseentriesItem>::new()
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
    pub async fn grants_get(
        &self,
        input: &rpc::GrantsGetInput,
    ) -> Result<rpc::GrantsGetOutput, trellis_rs::client::CallError<rpc::GrantsGetError>> {
        self.inner.call::<rpc::GrantsGet>(input).await
    }
    pub async fn grants_list(
        &self,
        input: &rpc::GrantsListInput,
    ) -> Result<rpc::GrantsListOutput, trellis_rs::client::CallError<rpc::GrantsListError>> {
        self.inner.call::<rpc::GrantsList>(input).await
    }
    pub fn grants_list_pages(
        &self,
        input: rpc::GrantsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::GrantsListOutput, crate::PaginationError<rpc::GrantsListError>>,
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
                    .grants_list(&input)
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
    pub fn grants_list_items(
        &self,
        input: rpc::GrantsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthGrantBinding,
            crate::PaginationError<rpc::GrantsListError>,
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
                Vec::<crate::__types::trellis::AuthGrantBinding>::new().into_iter(),
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
                        .grants_list(&input)
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
    pub async fn grants_revoke(
        &self,
        input: &rpc::GrantsRevokeInput,
    ) -> Result<rpc::GrantsRevokeOutput, trellis_rs::client::CallError<rpc::GrantsRevokeError>>
    {
        self.inner.call::<rpc::GrantsRevoke>(input).await
    }
    pub async fn grants_set(
        &self,
        input: &rpc::GrantsSetInput,
    ) -> Result<rpc::GrantsSetOutput, trellis_rs::client::CallError<rpc::GrantsSetError>> {
        self.inner.call::<rpc::GrantsSet>(input).await
    }
    pub async fn issuers_revoke(
        &self,
        input: &rpc::IssuersRevokeInput,
    ) -> Result<rpc::IssuersRevokeOutput, trellis_rs::client::CallError<rpc::IssuersRevokeError>>
    {
        self.inner.call::<rpc::IssuersRevoke>(input).await
    }
    pub async fn participants_get(
        &self,
        input: &rpc::ParticipantsGetInput,
    ) -> Result<rpc::ParticipantsGetOutput, trellis_rs::client::CallError<rpc::ParticipantsGetError>>
    {
        self.inner.call::<rpc::ParticipantsGet>(input).await
    }
    pub async fn participants_install(
        &self,
        input: &rpc::ParticipantsInstallInput,
    ) -> Result<
        rpc::ParticipantsInstallOutput,
        trellis_rs::client::CallError<rpc::ParticipantsInstallError>,
    > {
        self.inner.call::<rpc::ParticipantsInstall>(input).await
    }
    pub async fn participants_list(
        &self,
        input: &rpc::ParticipantsListInput,
    ) -> Result<
        rpc::ParticipantsListOutput,
        trellis_rs::client::CallError<rpc::ParticipantsListError>,
    > {
        self.inner.call::<rpc::ParticipantsList>(input).await
    }
    pub fn participants_list_pages(
        &self,
        input: rpc::ParticipantsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::ParticipantsListOutput, crate::PaginationError<rpc::ParticipantsListError>>,
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
                    .participants_list(&input)
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
    pub fn participants_list_items(
        &self,
        input: rpc::ParticipantsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::InstalledParticipantSummary,
            crate::PaginationError<rpc::ParticipantsListError>,
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
                Vec::<crate::__types::trellis::InstalledParticipantSummary>::new().into_iter(),
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
                        .participants_list(&input)
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
    pub async fn portals_get(
        &self,
        input: &rpc::PortalsGetInput,
    ) -> Result<rpc::PortalsGetOutput, trellis_rs::client::CallError<rpc::PortalsGetError>> {
        self.inner.call::<rpc::PortalsGet>(input).await
    }
    pub async fn portals_grant_overrides_list(
        &self,
        input: &rpc::PortalsGrantOverridesListInput,
    ) -> Result<
        rpc::PortalsGrantOverridesListOutput,
        trellis_rs::client::CallError<rpc::PortalsGrantOverridesListError>,
    > {
        self.inner
            .call::<rpc::PortalsGrantOverridesList>(input)
            .await
    }
    pub fn portals_grant_overrides_list_pages(
        &self,
        input: rpc::PortalsGrantOverridesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            rpc::PortalsGrantOverridesListOutput,
            crate::PaginationError<rpc::PortalsGrantOverridesListError>,
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
                    .portals_grant_overrides_list(&input)
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
    pub fn portals_grant_overrides_list_items(
        &self,
        input: rpc::PortalsGrantOverridesListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthPortalsGrantOverridesListResponseentriesItem,
            crate::PaginationError<rpc::PortalsGrantOverridesListError>,
        >,
    > {
        let client = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        if let Some(cursor) = input.page.as_ref().and_then(|page| page.cursor.clone()) {
            seen.insert(cursor);
        }
        Box::pin(
            futures_util::stream::try_unfold(
                (
                    client,
                    input,
                    seen,
                    false,
                    Vec::<
                        crate::__types::trellis::AuthPortalsGrantOverridesListResponseentriesItem,
                    >::new()
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
                            .portals_grant_overrides_list(&input)
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
            ),
        )
    }
    pub async fn portals_grant_overrides_put(
        &self,
        input: &rpc::PortalsGrantOverridesPutInput,
    ) -> Result<
        rpc::PortalsGrantOverridesPutOutput,
        trellis_rs::client::CallError<rpc::PortalsGrantOverridesPutError>,
    > {
        self.inner
            .call::<rpc::PortalsGrantOverridesPut>(input)
            .await
    }
    pub async fn portals_grant_overrides_remove(
        &self,
        input: &rpc::PortalsGrantOverridesRemoveInput,
    ) -> Result<
        rpc::PortalsGrantOverridesRemoveOutput,
        trellis_rs::client::CallError<rpc::PortalsGrantOverridesRemoveError>,
    > {
        self.inner
            .call::<rpc::PortalsGrantOverridesRemove>(input)
            .await
    }
    pub async fn portals_list(
        &self,
        input: &rpc::PortalsListInput,
    ) -> Result<rpc::PortalsListOutput, trellis_rs::client::CallError<rpc::PortalsListError>> {
        self.inner.call::<rpc::PortalsList>(input).await
    }
    pub fn portals_list_pages(
        &self,
        input: rpc::PortalsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rpc::PortalsListOutput, crate::PaginationError<rpc::PortalsListError>>,
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
                    .portals_list(&input)
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
    pub fn portals_list_items(
        &self,
        input: rpc::PortalsListInput,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<
            crate::__types::trellis::AuthPortalsListResponseentriesItem,
            crate::PaginationError<rpc::PortalsListError>,
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
                Vec::<crate::__types::trellis::AuthPortalsListResponseentriesItem>::new()
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
                        .portals_list(&input)
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
    pub async fn portals_login_settings_get(
        &self,
        input: &rpc::PortalsLoginSettingsGetInput,
    ) -> Result<
        rpc::PortalsLoginSettingsGetOutput,
        trellis_rs::client::CallError<rpc::PortalsLoginSettingsGetError>,
    > {
        self.inner.call::<rpc::PortalsLoginSettingsGet>(input).await
    }
    pub async fn portals_login_settings_update(
        &self,
        input: &rpc::PortalsLoginSettingsUpdateInput,
    ) -> Result<
        rpc::PortalsLoginSettingsUpdateOutput,
        trellis_rs::client::CallError<rpc::PortalsLoginSettingsUpdateError>,
    > {
        self.inner
            .call::<rpc::PortalsLoginSettingsUpdate>(input)
            .await
    }
    pub async fn portals_put(
        &self,
        input: &rpc::PortalsPutInput,
    ) -> Result<rpc::PortalsPutOutput, trellis_rs::client::CallError<rpc::PortalsPutError>> {
        self.inner.call::<rpc::PortalsPut>(input).await
    }
    pub async fn portals_remove(
        &self,
        input: &rpc::PortalsRemoveInput,
    ) -> Result<rpc::PortalsRemoveOutput, trellis_rs::client::CallError<rpc::PortalsRemoveError>>
    {
        self.inner.call::<rpc::PortalsRemove>(input).await
    }
    pub async fn portals_routes_put(
        &self,
        input: &rpc::PortalsRoutesPutInput,
    ) -> Result<
        rpc::PortalsRoutesPutOutput,
        trellis_rs::client::CallError<rpc::PortalsRoutesPutError>,
    > {
        self.inner.call::<rpc::PortalsRoutesPut>(input).await
    }
    pub async fn portals_routes_remove(
        &self,
        input: &rpc::PortalsRoutesRemoveInput,
    ) -> Result<
        rpc::PortalsRoutesRemoveOutput,
        trellis_rs::client::CallError<rpc::PortalsRoutesRemoveError>,
    > {
        self.inner.call::<rpc::PortalsRoutesRemove>(input).await
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
            crate::__types::trellis::AuthServiceInstancesListResponseentriesItem,
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
                Vec::<crate::__types::trellis::AuthServiceInstancesListResponseentriesItem>::new()
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
            crate::__types::trellis::AuthUserIdentitiesListResponseentriesItem,
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
                Vec::<crate::__types::trellis::AuthUserIdentitiesListResponseentriesItem>::new()
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
        Result<
            crate::__types::trellis::AuthUsersListResponseentriesItem,
            crate::PaginationError<rpc::UsersListError>,
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
                Vec::<crate::__types::trellis::AuthUsersListResponseentriesItem>::new().into_iter(),
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
    pub fn device_user_authorities_resolve(
        &self,
    ) -> trellis_rs::generated::Operation<'_, operations::DeviceUserAuthoritiesResolve> {
        self.inner
            .operation::<operations::DeviceUserAuthoritiesResolve>()
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
    pub async fn publish_device_user_authorities_approved(
        &self,
        event: &events::DeviceUserAuthoritiesApprovedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner
            .publish::<events::DeviceUserAuthoritiesApproved>(event)
            .await
    }
    pub async fn subscribe_device_user_authorities_approved(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<
                events::DeviceUserAuthoritiesApprovedEvent,
                trellis_rs::client::TrellisClientError,
            >,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::DeviceUserAuthoritiesApproved>(options)
            .await
    }
    pub async fn publish_device_user_authorities_requested(
        &self,
        event: &events::DeviceUserAuthoritiesRequestedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner
            .publish::<events::DeviceUserAuthoritiesRequested>(event)
            .await
    }
    pub async fn subscribe_device_user_authorities_requested(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<
                events::DeviceUserAuthoritiesRequestedEvent,
                trellis_rs::client::TrellisClientError,
            >,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::DeviceUserAuthoritiesRequested>(options)
            .await
    }
    pub async fn publish_device_user_authorities_resolved(
        &self,
        event: &events::DeviceUserAuthoritiesResolvedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner
            .publish::<events::DeviceUserAuthoritiesResolved>(event)
            .await
    }
    pub async fn subscribe_device_user_authorities_resolved(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<
                events::DeviceUserAuthoritiesResolvedEvent,
                trellis_rs::client::TrellisClientError,
            >,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::DeviceUserAuthoritiesResolved>(options)
            .await
    }
    pub async fn publish_device_user_authorities_review_requested(
        &self,
        event: &events::DeviceUserAuthoritiesReviewRequestedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner
            .publish::<events::DeviceUserAuthoritiesReviewRequested>(event)
            .await
    }
    pub async fn subscribe_device_user_authorities_review_requested(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<
                events::DeviceUserAuthoritiesReviewRequestedEvent,
                trellis_rs::client::TrellisClientError,
            >,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::DeviceUserAuthoritiesReviewRequested>(options)
            .await
    }
    pub async fn publish_grants_changed(
        &self,
        event: &events::GrantsChangedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::GrantsChanged>(event).await
    }
    pub async fn subscribe_grants_changed(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::GrantsChangedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner.subscribe::<events::GrantsChanged>(options).await
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
    pub async fn publish_sessions_revoked(
        &self,
        event: &events::SessionsRevokedEvent,
    ) -> Result<(), trellis_rs::client::TrellisClientError> {
        self.inner.publish::<events::SessionsRevoked>(event).await
    }
    pub async fn subscribe_sessions_revoked(
        &self,
        options: trellis_rs::client::EventSubscribeOptions,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<events::SessionsRevokedEvent, trellis_rs::client::TrellisClientError>,
        >,
        trellis_rs::client::TrellisClientError,
    > {
        self.inner
            .subscribe::<events::SessionsRevoked>(options)
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
    pub fn register_capability_groups_delete<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGroupsDeleteInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGroupsDeleteOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGroupsDelete, _, _>(handler);
    }
    pub fn register_capability_groups_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGroupsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGroupsGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGroupsGet, _, _>(handler);
    }
    pub fn register_capability_groups_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGroupsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGroupsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGroupsList, _, _>(handler);
    }
    pub fn register_capability_groups_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::CapabilityGroupsPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::CapabilityGroupsPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::CapabilityGroupsPut, _, _>(handler);
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
    pub fn register_device_user_authorities_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::DeviceUserAuthoritiesListInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeviceUserAuthoritiesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeviceUserAuthoritiesList, _, _>(handler);
    }
    pub fn register_device_user_authorities_reviews_decide<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::DeviceUserAuthoritiesReviewsDecideInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<
                    rpc::DeviceUserAuthoritiesReviewsDecideOutput,
                >,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeviceUserAuthoritiesReviewsDecide, _, _>(handler);
    }
    pub fn register_device_user_authorities_reviews_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::DeviceUserAuthoritiesReviewsListInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<
                    rpc::DeviceUserAuthoritiesReviewsListOutput,
                >,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeviceUserAuthoritiesReviewsList, _, _>(handler);
    }
    pub fn register_device_user_authorities_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::DeviceUserAuthoritiesRevokeInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::DeviceUserAuthoritiesRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::DeviceUserAuthoritiesRevoke, _, _>(handler);
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
    pub fn register_grants_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::GrantsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::GrantsGetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::GrantsGet, _, _>(handler);
    }
    pub fn register_grants_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::GrantsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::GrantsListOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::GrantsList, _, _>(handler);
    }
    pub fn register_grants_revoke<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::GrantsRevokeInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::GrantsRevokeOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::GrantsRevoke, _, _>(handler);
    }
    pub fn register_grants_set<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::GrantsSetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::GrantsSetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::GrantsSet, _, _>(handler);
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
    pub fn register_participants_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ParticipantsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ParticipantsGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ParticipantsGet, _, _>(handler);
    }
    pub fn register_participants_install<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ParticipantsInstallInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ParticipantsInstallOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ParticipantsInstall, _, _>(handler);
    }
    pub fn register_participants_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::ParticipantsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::ParticipantsListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::ParticipantsList, _, _>(handler);
    }
    pub fn register_portals_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::PortalsGetOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::PortalsGet, _, _>(handler);
    }
    pub fn register_portals_grant_overrides_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PortalsGrantOverridesListInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsGrantOverridesListOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsGrantOverridesList, _, _>(handler);
    }
    pub fn register_portals_grant_overrides_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PortalsGrantOverridesPutInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsGrantOverridesPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsGrantOverridesPut, _, _>(handler);
    }
    pub fn register_portals_grant_overrides_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PortalsGrantOverridesRemoveInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsGrantOverridesRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsGrantOverridesRemove, _, _>(handler);
    }
    pub fn register_portals_list<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsListInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::PortalsListOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::PortalsList, _, _>(handler);
    }
    pub fn register_portals_login_settings_get<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsLoginSettingsGetInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsLoginSettingsGetOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsLoginSettingsGet, _, _>(handler);
    }
    pub fn register_portals_login_settings_update<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::ServiceHandlerContext,
                rpc::PortalsLoginSettingsUpdateInput,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsLoginSettingsUpdateOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsLoginSettingsUpdate, _, _>(handler);
    }
    pub fn register_portals_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = trellis_rs::service::HandlerResult<rpc::PortalsPutOutput>>
            + Send
            + 'static,
    {
        self.runtime.register_rpc::<rpc::PortalsPut, _, _>(handler);
    }
    pub fn register_portals_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsRemoveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsRemove, _, _>(handler);
    }
    pub fn register_portals_routes_put<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsRoutesPutInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsRoutesPutOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsRoutesPut, _, _>(handler);
    }
    pub fn register_portals_routes_remove<F, Fut>(&mut self, handler: F)
    where
        F: Fn(trellis_rs::service::ServiceHandlerContext, rpc::PortalsRoutesRemoveInput) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<
                Output = trellis_rs::service::HandlerResult<rpc::PortalsRoutesRemoveOutput>,
            > + Send
            + 'static,
    {
        self.runtime
            .register_rpc::<rpc::PortalsRoutesRemove, _, _>(handler);
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
    pub fn register_device_user_authorities_resolve<F, Fut>(&mut self, handler: F)
    where
        F: Fn(
                trellis_rs::service::RequestContext,
                operations::DeviceUserAuthoritiesResolveInput,
                trellis_rs::service::OperationControl<
                    trellis_rs::generated::OperationAdapter<
                        operations::DeviceUserAuthoritiesResolve,
                    >,
                >,
            ) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = Result<(), trellis_rs::service::ServerError>>
            + Send
            + 'static,
    {
        self.runtime
            .register_operation_handler::<
                trellis_rs::generated::OperationAdapter<
                    operations::DeviceUserAuthoritiesResolve,
                >,
                F,
                Fut,
            >(handler);
    }
}
