//! Transport-independent native-IDL model codecs and wire values.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[path = "model_scalars.rs"]
mod scalars;
pub use scalars::{
    present_optional, Bytes, CursorPage, CursorPageInfo, CursorQuery, Int64, Nullable, Number,
    Uint64,
};

/// Native package identity for compiled models, not authority evidence.
#[derive(Clone, Copy, Debug)]
pub struct ModelPackage {
    /// Native package identity.
    pub name: &'static str,
    /// Authored package version. The semantic digest excludes it, so it only
    /// labels the snapshot.
    pub version: &'static str,
    /// Semantic digest including pinned dependency identities.
    pub digest: &'static str,
}

/// Reject a generated binding to different compiled models at compile time.
///
/// Models are the same when package identity and semantic digest match. The
/// authored versions may differ: a later model crate release that carries an
/// unchanged snapshot provides the same models.
pub const fn assert_model_binding(expected: ModelPackage, actual: ModelPackage) {
    let expected = [expected.name, expected.digest];
    let actual = [actual.name, actual.digest];
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

/// An IDL `ulid`: a canonical uppercase ULID, optionally after the one- or
/// two-letter prefix that names its kind, such as `U` for a user.
///
/// It reads and writes its text unchanged, so an ID made before prefixes,
/// a bare ULID, stays bare. Every character is an uppercase letter or a
/// digit, so an ID is always one NATS subject token.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Ulid {
    /// The prefix's letters, padded with zeros, which no prefix holds.
    prefix: [u8; 2],
    value: ulid::Ulid,
}
impl Ulid {
    /// Parse a canonical ULID with an optional prefix of one or two
    /// uppercase letters.
    pub fn parse(value: &str) -> Result<Self, UlidError> {
        let split = value.len().saturating_sub(ULID_LENGTH);
        let (prefix, rest) = value
            .split_at_checked(split)
            .ok_or(UlidError::NonCanonical)?;
        if prefix.len() > 2 || !prefix.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(UlidError::Prefix);
        }
        let parsed = rest.parse::<ulid::Ulid>().map_err(UlidError::Invalid)?;
        if parsed.to_string() != rest {
            return Err(UlidError::NonCanonical);
        }
        let mut letters = [0; 2];
        letters[..prefix.len()].copy_from_slice(prefix.as_bytes());
        Ok(Self {
            prefix: letters,
            value: parsed,
        })
    }

    /// Generate a new bare ULID from the current time and system randomness.
    pub fn generate() -> Self {
        Self {
            prefix: [0; 2],
            value: ulid::Ulid::generate(),
        }
    }

    /// Generate a new ID of `kind`: its prefix, then a new ULID.
    pub fn generate_for(kind: IdKind) -> Self {
        let mut letters = [0; 2];
        letters[..kind.prefix().len()].copy_from_slice(kind.prefix().as_bytes());
        Self {
            prefix: letters,
            value: ulid::Ulid::generate(),
        }
    }

    /// The prefix naming the ID's kind; empty for a bare ULID.
    pub fn prefix(&self) -> &str {
        let length = self.prefix.iter().take_while(|byte| **byte != 0).count();
        std::str::from_utf8(&self.prefix[..length]).unwrap_or_default()
    }

    /// Whether this is an ID of one of `kinds`: a bare ULID, or one with
    /// the prefix of one of them.
    pub fn is_one_of(&self, kinds: &[IdKind]) -> bool {
        let prefix = self.prefix();
        prefix.is_empty() || kinds.iter().any(|kind| kind.prefix() == prefix)
    }
}
/// The length of a canonical ULID.
const ULID_LENGTH: usize = 26;
impl std::fmt::Display for Ulid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.prefix())?;
        self.value.fmt(formatter)
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
    /// What comes before the ULID is not one or two uppercase letters.
    #[error("an ID's prefix is one or two uppercase letters")]
    Prefix,
    /// The prefix names another kind of ID.
    #[error("{0} is not {1}")]
    Kind(String, &'static str),
}

/// A kind of ID Trellis makes, each with its own prefix.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum IdKind {
    /// A user (`U`).
    User,
    /// A service deployment (`S`).
    ServiceDeployment,
    /// A service instance (`SI`).
    ServiceInstance,
    /// A device deployment (`D`).
    DeviceDeployment,
    /// A device instance (`DI`).
    DeviceInstance,
    /// An app, an OAuth client (`A`).
    App,
    /// A role (`R`).
    Role,
    /// A sign-in or authorization session (`SE`).
    Session,
    /// A job (`J`).
    Job,
    /// An operation (`O`).
    Operation,
    /// The principal a service or device instance acts as (`P`).
    InstancePrincipal,
    /// An enrollment secret or device provisioning secret (`E`).
    EnrollmentSecret,
    /// A service claim (`C`).
    ServiceClaim,
    /// A service update waiting for a decision (`SU`).
    ServiceUpdate,
    /// A device activation review or API review (`RV`).
    Review,
    /// A capability granted directly (`G`).
    Grant,
    /// A role given to a user (`RA`).
    RoleAssignment,
    /// An OIDC provider (`OP`).
    OidcProvider,
    /// A mapping from an OIDC claim to roles (`OM`).
    OidcRoleMapping,
    /// A sign-in linked to a user (`L`).
    LinkedSignIn,
    /// A service export (`X`).
    Export,
}
impl IdKind {
    /// The prefix this kind's IDs start with.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::User => "U",
            Self::ServiceDeployment => "S",
            Self::ServiceInstance => "SI",
            Self::DeviceDeployment => "D",
            Self::DeviceInstance => "DI",
            Self::App => "A",
            Self::Role => "R",
            Self::Session => "SE",
            Self::Job => "J",
            Self::Operation => "O",
            Self::InstancePrincipal => "P",
            Self::EnrollmentSecret => "E",
            Self::ServiceClaim => "C",
            Self::ServiceUpdate => "SU",
            Self::Review => "RV",
            Self::Grant => "G",
            Self::RoleAssignment => "RA",
            Self::OidcProvider => "OP",
            Self::OidcRoleMapping => "OM",
            Self::LinkedSignIn => "L",
            Self::Export => "X",
        }
    }

    /// The kind's name in words, for an error.
    pub const fn noun(self) -> &'static str {
        match self {
            Self::User => "a user ID",
            Self::ServiceDeployment => "a service deployment ID",
            Self::ServiceInstance => "a service instance ID",
            Self::DeviceDeployment => "a device deployment ID",
            Self::DeviceInstance => "a device instance ID",
            Self::App => "an app ID",
            Self::Role => "a role ID",
            Self::Session => "a session ID",
            Self::Job => "a job ID",
            Self::Operation => "an operation ID",
            Self::InstancePrincipal => "an instance principal ID",
            Self::EnrollmentSecret => "an enrollment secret ID",
            Self::ServiceClaim => "a service claim ID",
            Self::ServiceUpdate => "a service update ID",
            Self::Review => "a review ID",
            Self::Grant => "a grant ID",
            Self::RoleAssignment => "a role assignment ID",
            Self::OidcProvider => "an OIDC provider ID",
            Self::OidcRoleMapping => "an OIDC role mapping ID",
            Self::LinkedSignIn => "a linked sign-in ID",
            Self::Export => "an export ID",
        }
    }

    /// A new ID of this kind, as text.
    pub fn generate(self) -> String {
        Ulid::generate_for(self).to_string()
    }
}

/// Parse `text`, an ID that came from outside, as an ID of one of `kinds`:
/// a bare ULID, as IDs made before prefixes are, or one with a prefix of one
/// of them. It is refused when it is anything else, so what it returns is
/// made only of uppercase letters and digits.
pub fn parse_id(text: &str, kinds: &[IdKind]) -> Result<Ulid, UlidError> {
    let id = Ulid::parse(text)?;
    if !id.is_one_of(kinds) {
        return Err(UlidError::Kind(
            text.to_owned(),
            kinds.first().map_or("an ID", |kind| kind.noun()),
        ));
    }
    Ok(id)
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

#[cfg(test)]
mod tests {
    use super::{parse_id, IdKind, Ulid};

    /// An ID from outside parses as the kinds it may be: bare, as rc.56
    /// and earlier made them, or with one of their prefixes, and its text
    /// comes back unchanged. Anything that could change a NATS subject, a
    /// lowercase spelling, or another kind's prefix is refused.
    #[test]
    fn an_id_parses_bare_or_with_its_kinds_prefix_only() {
        let bare = "01KQ8Z9Y2C3D4E5F6G7H8J9K0M";
        let deployments = [IdKind::ServiceDeployment, IdKind::DeviceDeployment];
        for text in [bare.to_owned(), format!("S{bare}"), format!("D{bare}")] {
            let parsed = parse_id(&text, &deployments).unwrap();
            assert_eq!(parsed.to_string(), text);
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                serde_json::Value::String(text)
            );
        }
        for text in [
            format!("U{bare}"),
            format!("SI{bare}"),
            format!("s{bare}"),
            bare.to_ascii_lowercase(),
            format!("S{bare}.>"),
            format!("S*{bare}"),
            format!("S {bare}"),
            format!("SXY{bare}"),
            format!("S{}", &bare[1..]),
            String::new(),
        ] {
            assert!(parse_id(&text, &deployments).is_err(), "{text:?} parsed");
        }
        // The IDL type takes any kind's prefix, and a new ID carries its own.
        let made = IdKind::ServiceInstance.generate();
        assert!(made.starts_with("SI") && made.len() == 28, "{made}");
        assert_eq!(Ulid::parse(&made).unwrap().prefix(), "SI");
        assert!(parse_id(&made, &[IdKind::ServiceInstance]).is_ok());
        assert!(parse_id(&made, &[IdKind::ServiceDeployment]).is_err());
    }
}
