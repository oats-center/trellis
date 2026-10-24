//! Shared native IDL inputs for Trellis SDK and runtime builds.

use std::path::Path;

/// Directory containing the authored built-in manifest and native IDL sources.
pub fn source_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Compile built-in contracts from their single authored source package.
///
/// # Errors
/// Returns source-loading or native IDL compilation errors.
pub fn compile() -> miette::Result<trellis_idl::PackageGraph> {
    let root = source_root();
    let manifest = trellis_idl::project::read_manifest(&root.join("trellis.toml"))?;
    let sources = trellis_idl::project::load_sources(root, &manifest)?;
    trellis_idl::compile_project(&manifest, sources, Default::default())
}
