//! Trellis CLI library entrypoints and shared support code.

#[cfg(feature = "runtime")]
pub mod app;
pub mod cli;
pub mod generate;
pub mod oci;
pub mod output;
pub mod package;
pub mod project;
#[cfg(feature = "runtime")]
pub mod self_update;
