//! Frozen models for one native-contract snapshot, independently versioned from the SDK.
//!
//! This crate contains no network clients, runtime, or compiler. Model snapshots
//! use distinct Cargo compatibility lines; patch releases retain their native
//! package identity and generated model ABI.

#[doc(hidden)]
pub mod __types {
    include!("../generated/__types.rs");
}

pub use __types::trellis;
