//! Build the runtime integration fixture without a pre-generated path dependency.

use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory")).join("../..");
    let manifest = trellis_idl::project::read_manifest(&root.join("trellis.toml"))?;
    println!(
        "cargo:rerun-if-changed={}",
        root.join("trellis.toml").display()
    );
    for path in manifest.sources.values() {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let sources = trellis_idl::project::load_sources(&root, &manifest)?;
    let builtin = trellis_builtin_contracts::compile()?;
    let binding = trellis_codegen_rust::RustModelBinding::from_package_manifest(
        &builtin,
        include_str!("../../../../../crates/models/Cargo.toml"),
    )?;
    let graph = trellis_idl::compile_project(
        &manifest,
        sources,
        std::collections::BTreeMap::from([("trellis".into(), builtin)]),
    )?;
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    let generated = out.join("generated");
    trellis_codegen_rust::generate_rust_package(
        &graph,
        &generated,
        &env::var("CARGO_PKG_NAME")?,
        &[binding],
    )?;
    fs::write(
        out.join("modules.rs"),
        format!(
            "#[path = {:?}] mod generated;\n",
            generated.join("src/lib.rs")
        ),
    )?;
    Ok(())
}
