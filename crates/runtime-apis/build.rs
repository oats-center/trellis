//! Build the typed runtime API facade from the shared native IDL package.

use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "cargo:rerun-if-changed={}",
        trellis_builtin_contracts::source_root().display()
    );
    let graph = trellis_builtin_contracts::compile()?;
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    let generated = out.join("generated");
    trellis_codegen_rust::generate_rust_package(
        &graph,
        &generated,
        "trellis-runtime-apis",
        &[
            trellis_codegen_rust::RustModelBinding::from_package_manifest(
                &graph,
                include_str!("../models/Cargo.toml"),
            )?,
        ],
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
