//! Build SDK-owned wire types and descriptors from the shared native IDL package.

use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "cargo:rerun-if-changed={}",
        trellis_builtin_contracts::source_root().display()
    );
    let graph = trellis_builtin_contracts::compile()?;
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    trellis_codegen_rust::generate_rust_package(
        &graph,
        &out.join("bindings"),
        "trellis-sdk-bindings",
    )?;
    let mut modules = format!(
        "#[doc(hidden)] #[path = {:?}] pub mod __types;\n",
        out.join("bindings/src/__types.rs")
    );
    for (module, api, kind, action, visibility) in [
        (
            "runtime_health",
            "trellis.health@v1",
            trellis_idl::ActionKind::Event,
            "Heartbeat",
            "",
        ),
        (
            "runtime_state",
            "trellis.state@v1",
            trellis_idl::ActionKind::Live,
            "Watch",
            "pub ",
        ),
    ] {
        let path = out.join(format!("{module}.rs"));
        fs::write(
            &path,
            trellis_codegen_rust::render_runtime_action(&graph, api, kind, action)?,
        )?;
        modules.push_str(&format!(
            "#[doc(hidden)] #[path = {path:?}] {visibility}mod {module};\n"
        ));
    }
    fs::write(out.join("modules.rs"), modules)?;
    Ok(())
}
