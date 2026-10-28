//! Canonical native-IDL scalar and cursor wire values.

use serde::{Deserialize, Serialize};

/// A nullable wire value, distinct from an omitted object field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Nullable<T> {
    /// An explicit JSON null.
    Null,
    /// A present non-null value.
    Value(T),
}

/// Deserialize a present optional field as `Some`, so an explicit JSON null in
/// an optional nullable field stays `Some(Nullable::Null)` instead of `None`.
///
/// Generated models use this with `#[serde(default, deserialize_with = ...)]`;
/// an omitted field still defaults to `None`.
pub fn present_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A signed integer encoded as a canonical decimal string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Int64(
    /// The native signed value.
    pub i64,
);

impl From<i64> for Int64 {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

impl From<Int64> for i64 {
    fn from(value: Int64) -> Self {
        value.0
    }
}

impl std::ops::Deref for Int64 {
    type Target = i64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for Int64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        super::serde_i64::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Int64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        super::serde_i64::deserialize(deserializer).map(Self)
    }
}

/// An unsigned integer encoded as a canonical decimal string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Uint64(
    /// The native unsigned value.
    pub u64,
);

impl From<u64> for Uint64 {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Uint64> for u64 {
    fn from(value: Uint64) -> Self {
        value.0
    }
}

impl std::ops::Deref for Uint64 {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for Uint64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        super::serde_u64::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Uint64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        super::serde_u64::deserialize(deserializer).map(Self)
    }
}

/// Binary bytes encoded as canonical padded base64.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bytes(
    /// The decoded bytes.
    pub Vec<u8>,
);

impl From<Vec<u8>> for Bytes {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        super::serde_base64::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        super::serde_base64::deserialize(deserializer).map(Self)
    }
}

/// A finite JSON number with canonical zero representation.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Number(
    /// The native floating-point value.
    pub f64,
);

impl From<f64> for Number {
    fn from(value: f64) -> Self {
        Self(value)
    }
}

impl From<Number> for f64 {
    fn from(value: Number) -> Self {
        value.0
    }
}

impl std::ops::Deref for Number {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for Number {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        super::serde_f64::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        super::serde_f64::deserialize(deserializer).map(Self)
    }
}

/// Parameters shared by cursor-paginated queries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorQuery {
    /// Opaque continuation cursor returned by the preceding page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Requested maximum page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One cursor page and its continuation metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPage<T> {
    /// Items in this page.
    pub items: Vec<T>,
    /// Continuation metadata.
    pub page: CursorPageInfo,
}

/// Continuation metadata returned by a cursor-paginated query.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPageInfo {
    /// Cursor for the next page, absent on the terminal page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
