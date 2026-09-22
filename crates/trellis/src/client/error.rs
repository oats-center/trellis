use serde_json::Value;

/// Structured payload returned by a remote RPC error response.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcErrorPayload {
    raw: String,
    value: Option<Value>,
}

/// Structured payload returned by an undeclared remote Trellis error.
pub type RemoteErrorPayload = RpcErrorPayload;

impl RpcErrorPayload {
    /// Builds a payload from a raw JSON RPC error body.
    pub fn from_json_slice(raw: &[u8]) -> Result<Self, serde_json::Error> {
        let value = serde_json::from_slice::<Value>(raw)?;
        Ok(Self {
            raw: String::from_utf8_lossy(raw).into_owned(),
            value: Some(value),
        })
    }

    /// Builds a payload from a decoded JSON RPC error body.
    pub fn from_value(value: Value) -> Self {
        Self {
            raw: value.to_string(),
            value: Some(value),
        }
    }

    /// Builds a payload from an unstructured error message.
    pub fn from_message(message: impl Into<String>) -> Self {
        Self {
            raw: message.into(),
            value: None,
        }
    }

    /// Returns the original payload text.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Returns the decoded JSON payload when the RPC error body was structured.
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    /// Returns the remote error discriminator when present.
    pub fn error_type(&self) -> Option<&str> {
        self.value
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
    }

    fn format_human(&self) -> String {
        if let Some(value) = &self.value {
            format_rpc_error_value(value, &self.raw)
        } else {
            self.raw.clone()
        }
    }
}

/// Authentication failure returned while making a connected call.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct AuthenticationError {
    message: String,
}

impl AuthenticationError {
    /// Build an authentication error from a runtime message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Remote protocol failure, including malformed contract payloads.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ProtocolError {
    message: String,
}

impl ProtocolError {
    /// Build a protocol error from a runtime message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Connected transport failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct TransportError {
    message: String,
}

impl TransportError {
    /// Build a transport error from a runtime message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Errors returned by generated caller methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CallError<E>
where
    E: std::fmt::Debug,
{
    /// Contract-declared error.
    #[error("declared remote error: {0:?}")]
    Declared(Box<E>),
    /// Well-formed remote error not declared by this action.
    #[error("remote error: {}", .0.format_human())]
    Remote(RemoteErrorPayload),
    /// Request timeout.
    #[error("request timeout")]
    Timeout,
    /// Authentication failure.
    #[error(transparent)]
    Authentication(AuthenticationError),
    /// Invalid protocol frame or contract payload.
    #[error(transparent)]
    Protocol(ProtocolError),
    /// NATS or other connected transport failure.
    #[error(transparent)]
    Transport(TransportError),
    /// An optional generated action is not present in the installed availability snapshot.
    #[error("optional action unavailable: {0}")]
    AuthorizationUnavailable(String),
}

impl<E> CallError<E>
where
    E: std::fmt::Debug,
{
    pub(crate) fn from_client(
        error: TrellisClientError,
        decode_error: impl FnOnce(Value) -> Result<Option<E>, serde_json::Error>,
    ) -> Self {
        match error {
            TrellisClientError::RpcError(payload) => match payload.value().cloned() {
                Some(value) => match decode_error(value) {
                    Ok(Some(error)) => Self::Declared(Box::new(error)),
                    Ok(None) => Self::Remote(payload),
                    Err(error) => Self::Protocol(ProtocolError::new(error.to_string())),
                },
                None => Self::Protocol(ProtocolError::new(
                    "remote error payload is not structured JSON",
                )),
            },
            TrellisClientError::Timeout => Self::Timeout,
            TrellisClientError::Json(error) => {
                Self::Protocol(ProtocolError::new(error.to_string()))
            }
            TrellisClientError::Codec(error) => Self::Protocol(ProtocolError::new(error)),
            TrellisClientError::AuthorizationUnavailable(message) => {
                Self::AuthorizationUnavailable(message)
            }
            error => Self::Transport(TransportError::new(error.to_string())),
        }
    }
}

fn format_json_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values
            .iter()
            .map(format_json_value)
            .collect::<Vec<_>>()
            .join(","),
        Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

fn format_issue(issue: &Value) -> Option<String> {
    let obj = issue.as_object()?;
    let message = obj.get("message")?.as_str()?.trim();
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches('/');

    if path.is_empty() || message.contains(path) {
        Some(message.to_string())
    } else {
        Some(format!("{path}: {message}"))
    }
}

fn format_context(value: &Value) -> Option<String> {
    let obj = value.as_object()?;
    let fields = obj
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| format!("{key}={}", format_json_value(value)))
        .collect::<Vec<_>>();
    if fields.is_empty() {
        None
    } else {
        Some(fields.join(", "))
    }
}

fn format_rpc_error_value(value: &Value, raw: &str) -> String {
    let issues = value
        .get("issues")
        .and_then(Value::as_array)
        .map(|issues| issues.iter().filter_map(format_issue).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut message = if issues.is_empty() {
        value
            .get("message")
            .and_then(Value::as_str)
            .map(|message| {
                message
                    .strip_prefix("Validation failed. ")
                    .unwrap_or(message)
                    .to_string()
            })
            .unwrap_or_else(|| raw.to_string())
    } else {
        issues.join("; ")
    };

    if let Some(context) = value.get("context").and_then(format_context) {
        message.push_str(&format!(" ({context})"));
    }

    message
}

#[cfg(test)]
fn format_rpc_error_payload(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };

    format_rpc_error_value(&value, raw)
}

/// Errors returned by the Trellis client runtime.
#[derive(thiserror::Error, Debug)]
pub enum TrellisClientError {
    #[error("invalid base64url: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("invalid ed25519 seed length: {0} (expected 32)")]
    InvalidSeedLen(usize),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("nats error: {0}")]
    Nats(#[from] async_nats::Error),

    #[error("nats connect error: {0}")]
    NatsConnect(String),

    #[error("nats request error: {0}")]
    NatsRequest(String),

    #[error("Trellis HTTP request failed with status {status}: {code}")]
    BootstrapHttp { status: u16, code: String },

    /// Required authorization evidence could not be obtained or kept current.
    #[error("authorization evidence unavailable: {0}")]
    AuthorizationUnavailable(String),

    #[error("service bootstrap error: {0}")]
    Bootstrap(String),

    #[error("request timeout")]
    Timeout,

    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),

    /// A generated wire codec rejected a value.
    #[error("generated codec error: {0}")]
    Codec(String),

    #[error(transparent)]
    Subject(#[from] super::subject::SubjectError),

    #[error("rpc returned error: {}", .0.format_human())]
    RpcError(RpcErrorPayload),

    #[error("operation protocol error: {0}")]
    OperationProtocol(String),

    #[error("transfer protocol error: {0}")]
    TransferProtocol(String),

    #[error("transfer cancelled")]
    TransferCancelled,

    #[error("event subscription protocol error: {0}")]
    EventSubscriptionProtocol(String),

    #[error("feed protocol error: {0}")]
    FeedProtocol(String),
}

#[cfg(test)]
mod tests {
    use super::{format_rpc_error_payload, RpcErrorPayload};

    #[test]
    fn formats_validation_error_payload_human_readably() {
        let raw = r#"{"context":{"deploymentId":"demo"},"issues":[{"message":"service deployment not found","path":"/deploymentId"}],"message":"Validation failed. /deploymentId: service deployment not found.","type":"ValidationError"}"#;
        assert_eq!(
            format_rpc_error_payload(raw),
            "deploymentId: service deployment not found (deploymentId=demo)"
        );
    }

    #[test]
    fn leaves_non_json_payloads_unchanged() {
        assert_eq!(format_rpc_error_payload("plain error"), "plain error");
    }

    #[test]
    fn rpc_error_payload_preserves_structured_error_type() {
        let raw = r#"{"type":"UnexpectedError","message":"rust handler error marker"}"#;
        let payload = RpcErrorPayload::from_json_slice(raw.as_bytes()).unwrap();

        assert_eq!(payload.raw(), raw);
        assert_eq!(payload.error_type(), Some("UnexpectedError"));
    }

    #[test]
    fn rpc_error_display_uses_formatted_payload() {
        let error =
            super::TrellisClientError::RpcError(RpcErrorPayload::from_value(serde_json::json!({
                "context": { "deploymentId": "demo" },
                "issues": [{ "message": "service deployment not found", "path": "/deploymentId" }],
                "message": "Validation failed. /deploymentId: service deployment not found.",
                "type": "ValidationError"
            })));

        assert_eq!(
            error.to_string(),
            "rpc returned error: deploymentId: service deployment not found (deploymentId=demo)"
        );
    }
}
