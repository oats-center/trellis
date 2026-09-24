//! Generated Trellis APIs, participants, and wire types.
const _: () = trellis_rs::generated::assert_abi(1);
/// Failure while traversing a cursor-paginated RPC.
#[derive(Debug)]
pub enum PaginationError<E: std::fmt::Debug> {
    /// The RPC call failed.
    Call(trellis_rs::client::CallError<E>),
    /// The server returned a cursor already seen by this traversal.
    RepeatedCursor(String),
}
impl<E: std::fmt::Debug> std::fmt::Display for PaginationError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Call(error) => error.fmt(formatter),
            Self::RepeatedCursor(cursor) => {
                write!(formatter, "server repeated pagination cursor '{cursor}'")
            }
        }
    }
}
impl<E: std::fmt::Debug> std::error::Error for PaginationError<E> {}
impl<E: std::fmt::Debug> From<trellis_rs::client::CallError<E>> for PaginationError<E> {
    fn from(error: trellis_rs::client::CallError<E>) -> Self {
        Self::Call(error)
    }
}
#[doc(hidden)]
pub mod __types;
pub mod apis;
pub mod participants;
pub mod types;
pub use __types::CursorQuery;
pub use types::*;
