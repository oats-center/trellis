//! Generates the compile-time index for the embedded Trellis web application.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read embedded portal directory") {
        let path = entry.expect("read embedded portal entry").path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn generate_index(root: &Path, output_name: &str) {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    if root.is_dir() {
        collect_files(root, &mut files);
    }
    files.sort();
    let entries = files
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .expect("embedded application path below root")
                .to_string_lossy()
                .replace('\\', "/");
            format!(
                "({relative:?}, include_bytes!({path:?}).as_slice()),",
                path = path.canonicalize().expect("canonical application path")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("build output directory")).join(output_name);
    fs::write(output, format!("&[{entries}]\n")).expect("write embedded application index");
}

fn main() {
    let generated = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("generated");
    generate_index(&generated.join("web"), "web_assets.rs");
}
