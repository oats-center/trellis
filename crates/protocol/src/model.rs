//! Transport-independent native-IDL model codecs and wire values.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[path = "model_scalars.rs"]
mod scalars;
pub use scalars::{
    present_optional, Bytes, CursorPage, CursorPageInfo, CursorQuery, Int64, Nullable, Number,
    Uint64,
};

/// Exact native package identity for compiled models, not authority evidence.
#[derive(Clone, Copy, Debug)]
pub struct ModelPackage {
    /// Rust model representation ABI, independent of transport execution APIs.
    pub rust_abi: u32,
    /// Native package identity.
    pub name: &'static str,
    /// Authored package version.
    pub version: &'static str,
    /// Semantic digest including pinned dependency identities.
    pub digest: &'static str,
}

/// Reject a generated binding to different compiled models at compile time.
pub const fn assert_model_binding(expected: ModelPackage, actual: ModelPackage) {
    assert!(
        expected.rust_abi == actual.rust_abi,
        "generated Trellis model ABI mismatch"
    );
    let expected = [expected.name, expected.version, expected.digest];
    let actual = [actual.name, actual.version, actual.digest];
    let mut part = 0;
    while part < expected.len() {
        let left = expected[part].as_bytes();
        let right = actual[part].as_bytes();
        assert!(
            left.len() == right.len(),
            "generated Trellis model binding mismatch"
        );
        let mut index = 0;
        while index < left.len() {
            assert!(
                left[index] == right[index],
                "generated Trellis model binding mismatch"
            );
            index += 1;
        }
        part += 1;
    }
}

/// Failure while converting a generated wire value.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The JSON representation did not match the generated Rust type.
    #[error("invalid generated wire value: {0}")]
    Json(#[from] serde_json::Error),
}

/// Bidirectional JSON codec shared by generated models across SDK releases.
pub trait Codec: Sized {
    /// Encode this value to its JSON wire representation.
    fn encode(&self) -> Result<serde_json::Value, CodecError>;
    /// Decode this value from its JSON wire representation.
    fn decode(value: serde_json::Value) -> Result<Self, CodecError>;
}

impl<T: Serialize + DeserializeOwned> Codec for T {
    fn encode(&self) -> Result<serde_json::Value, CodecError> {
        Ok(serde_json::to_value(self)?)
    }
    fn decode(value: serde_json::Value) -> Result<Self, CodecError> {
        Ok(serde_json::from_value(value)?)
    }
}

/// RFC 3339 timestamp normalized to canonical UTC.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(String);

impl Timestamp {
    /// Parse an RFC 3339 timestamp with up to nanosecond precision and normalize it.
    pub fn parse(value: &str) -> Result<Self, TimestampError> {
        use time::format_description::well_known::Rfc3339;
        if !matches!(value.as_bytes().get(10), Some(b'T' | b't')) {
            return Err(TimestampError::Separator);
        }
        if value.get(17..19) == Some("60") {
            return Err(TimestampError::LeapSecond);
        }
        if value
            .get(19..)
            .and_then(|tail| tail.strip_prefix('.'))
            .is_some_and(|fraction| fraction.bytes().take_while(u8::is_ascii_digit).count() > 9)
        {
            return Err(TimestampError::Precision);
        }
        let parsed = time::OffsetDateTime::parse(value, &Rfc3339)?;
        Ok(Self(
            parsed.to_offset(time::UtcOffset::UTC).format(&Rfc3339)?,
        ))
    }
    /// The canonical timestamp for whole Unix seconds.
    pub fn from_unix_seconds(seconds: i64) -> Result<Self, TimestampError> {
        use time::format_description::well_known::Rfc3339;
        let value = time::OffsetDateTime::from_unix_timestamp(seconds)
            .map_err(|_| TimestampError::Range)?;
        Ok(Self(value.format(&Rfc3339)?))
    }
    /// Borrow the canonical wire string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::str::FromStr for Timestamp {
    type Err = TimestampError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Invalid canonical timestamp.
#[derive(Debug, thiserror::Error)]
pub enum TimestampError {
    /// The value is not RFC 3339.
    #[error("invalid RFC 3339 timestamp: {0}")]
    Parse(#[from] time::error::Parse),
    /// A parsed timestamp could not be rendered.
    #[error("could not format RFC 3339 timestamp: {0}")]
    Format(#[from] time::error::Format),
    /// Fractional seconds exceed nanosecond precision.
    #[error("timestamp exceeds nanosecond precision")]
    Precision,
    /// A non-RFC 3339 separator was used.
    #[error("timestamp requires an RFC 3339 date/time separator")]
    Separator,
    /// Leap seconds cannot be represented without loss.
    #[error("leap seconds are not supported in timestamps")]
    LeapSecond,
    /// The instant is outside the representable calendar range.
    #[error("timestamp is outside the supported range")]
    Range,
}

/// Validated uppercase canonical ULID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Ulid(ulid::Ulid);
impl Ulid {
    /// Parse an uppercase canonical ULID.
    pub fn parse(value: &str) -> Result<Self, UlidError> {
        let parsed = value.parse::<ulid::Ulid>().map_err(UlidError::Invalid)?;
        if parsed.to_string() != value {
            return Err(UlidError::NonCanonical);
        }
        Ok(Self(parsed))
    }
}
impl std::fmt::Display for Ulid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
impl std::str::FromStr for Ulid {
    type Err = UlidError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl Serialize for Ulid {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Ulid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
/// Invalid or non-canonical ULID.
#[derive(Debug, thiserror::Error)]
pub enum UlidError {
    /// The value is not a ULID.
    #[error("invalid ULID: {0}")]
    Invalid(ulid::DecodeError),
    /// The value does not use uppercase canonical spelling.
    #[error("ULID must use uppercase canonical spelling")]
    NonCanonical,
}

/// Standard serializable fields carried by declared errors.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializableErrorData {
    /// Error instance identifier.
    pub id: String,
    /// Contract error discriminator.
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-facing error message.
    pub message: String,
    /// Optional structured context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Map<String, serde_json::Value>>,
    /// Optional distributed trace identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Additive fields from newer runtimes or declared errors.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
/// Contract implemented by generated typed errors.
pub trait TrellisError: std::error::Error + Serialize + DeserializeOwned {
    /// Stable contract error discriminator.
    const TYPE: &'static str;
}
/// Decode a typed error only when its discriminator matches.
pub fn decode_typed_error<E: TrellisError>(
    value: serde_json::Value,
) -> Result<Option<E>, serde_json::Error> {
    if value.get("type").and_then(serde_json::Value::as_str) != Some(E::TYPE) {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some)
}

/// Serde adapter for canonical decimal-string signed integers.
pub mod serde_i64 {
    use serde::{de::Error as _, Deserialize as _, Deserializer, Serializer};
    /// Serialize as a decimal JSON string.
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    /// Deserialize a canonical decimal JSON string.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let value = String::deserialize(deserializer)?;
        let digits = value.strip_prefix('-').unwrap_or(&value);
        if digits.is_empty()
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
            || value == "-0"
        {
            return Err(D::Error::custom("expected canonical i64 decimal string"));
        }
        value.parse().map_err(D::Error::custom)
    }
}
/// Serde adapter for canonical decimal-string unsigned integers.
pub mod serde_u64 {
    use serde::{de::Error as _, Deserialize as _, Deserializer, Serializer};
    /// Serialize as a decimal JSON string.
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    /// Deserialize a canonical decimal JSON string.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(D::Error::custom("expected canonical u64 decimal string"));
        }
        value.parse().map_err(D::Error::custom)
    }
}
/// Serde adapter rejecting non-finite JSON numbers.
pub mod serde_f64 {
    use serde::{de::Error as _, ser::Error as _, Deserialize as _, Deserializer, Serializer};
    /// Serialize a finite JSON number.
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if !value.is_finite() {
            return Err(S::Error::custom("expected finite f64"));
        }
        serializer.serialize_f64(*value)
    }
    /// Deserialize a finite JSON number.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        let value = f64::deserialize(deserializer)?;
        if !value.is_finite() {
            return Err(D::Error::custom("expected finite f64"));
        }
        Ok(value)
    }
}
/// Serde adapter for canonical padded standard-base64 bytes.
pub mod serde_base64 {
    use base64::Engine as _;
    use serde::{de::Error as _, Deserialize as _, Deserializer, Serializer};
    /// Serialize bytes as padded standard base64.
    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(value))
    }
    /// Deserialize canonical padded standard base64.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let value = String::deserialize(deserializer)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&value)
            .map_err(D::Error::custom)?;
        if base64::engine::general_purpose::STANDARD.encode(&decoded) != value {
            return Err(D::Error::custom(
                "expected canonical padded standard base64",
            ));
        }
        Ok(decoded)
    }
}
