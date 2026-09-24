use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result, WrapErr};

mod builtin_semantics;
mod release;

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
enum XtaskCommand {
    #[command(name = "install")]
    Install {
        #[arg(long)]
        update: bool,
    },
    #[command(name = "protocol-wasm")]
    ProtocolWasm,
    #[command(name = "build", disable_help_flag = true)]
    Build {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(name = "release")]
    Release {
        #[command(subcommand)]
        command: release::ReleaseCommand,
    },
}

#[derive(Debug, Parser)]
#[command(name = "cargo xtask")]
struct XtaskCli {
    #[command(subcommand)]
    command: XtaskCommand,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let Some(command) = parse_command(env::args().skip(1))? else {
        return Ok(());
    };
    match command {
        XtaskCommand::Install { update } => run_install(update),
        XtaskCommand::ProtocolWasm => generate_protocol_wasm(),
        XtaskCommand::Build { args } => run_build(&args),
        XtaskCommand::Release { command } => release::run_release(&repo_root()?, command),
    }
}

fn parse_command<I>(args: I) -> Result<Option<XtaskCommand>>
where
    I: Iterator<Item = String>,
{
    let input_args = args.collect::<Vec<_>>();
    let build_delimiter_index = match input_args.first() {
        Some(command) if command == "build" => {
            input_args.iter().skip(1).position(|arg| arg == "--")
        }
        _ => None,
    };
    let argv = std::iter::once("cargo-xtask".to_string()).chain(input_args.iter().cloned());
    let mut command = match XtaskCli::try_parse_from(argv) {
        Ok(cli) => cli.command,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    | ErrorKind::DisplayVersion
            ) =>
        {
            error.print().into_diagnostic()?;
            return Ok(None);
        }
        Err(error) => return Err(miette::miette!("{error}")),
    };
    if let Some(delimiter_index) = build_delimiter_index {
        if let XtaskCommand::Build { args } = &mut command {
            if args.get(delimiter_index).map(String::as_str) != Some("--") {
                args.insert(delimiter_index, "--".to_string());
            }
        }
    }
    if let XtaskCommand::Release {
        command: release_command,
    } = &command
    {
        release::validate_release_command(release_command)?;
    }
    Ok(Some(command))
}

const TRELLIS_PROJECTS: &[&str] = &[
    // Trellis has a small fixed API DAG. Replace this list with dynamic graph
    // discovery only if maintaining it becomes a real problem.
    "ts/packages/trellis-test",
    "web",
    "integration/fixtures/runtime",
    "integration/fixtures/runtime-removed",
    "demos/ts/service",
    "demos/ts/device",
    "demos/app",
    "demos/rust/service",
    "demos/rust/device",
    "docs/examples/orders",
];

fn run_install(update: bool) -> Result<()> {
    let root = repo_root()?;
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    if update {
        runtime.block_on(trellis_cli::package::update(
            trellis_cli::cli::OutputFormat::Text,
            &trellis_cli::cli::UpdateArgs {
                dependency_alias: None,
                project: trellis_cli::cli::ProjectRootArgs {
                    root: root.join("crates/runtime"),
                },
            },
        ))?;
    }
    generate_builtin_package(&root).wrap_err("generating built-in package")?;
    project_testkit_admin_source(&root).wrap_err("projecting testkit administration source")?;
    for project in TRELLIS_PROJECTS {
        let project = root.join(project);
        let project_label = project.display().to_string();
        if update {
            runtime.block_on(trellis_cli::package::update(
                trellis_cli::cli::OutputFormat::Text,
                &trellis_cli::cli::UpdateArgs {
                    dependency_alias: None,
                    project: trellis_cli::cli::ProjectRootArgs { root: project },
                },
            ))?;
        } else {
            runtime
                .block_on(trellis_cli::package::install(
                    trellis_cli::cli::OutputFormat::Text,
                    &trellis_cli::cli::ProjectRootArgs { root: project },
                ))
                .wrap_err_with(|| format!("installing {project_label}"))?;
        }
    }
    std::fs::copy(
        root.join("crates/local-nats/nats-binaries.json"),
        root.join("ts/packages/trellis-test/src/nats-binaries.json"),
    )
    .into_diagnostic()?;
    Ok(())
}

fn generate_builtin_package(root: &Path) -> Result<()> {
    let project = root.join("crates/runtime");
    let manifest = trellis_idl::project::read_manifest(&project.join("trellis.toml"))?;
    let sources = trellis_idl::project::load_sources(&project, &manifest)?;
    let graph = trellis_idl::compile_project(&manifest, sources, Default::default())?;
    let builtin_semantics = builtin_semantics::render(&graph)?;
    let target = root.join("crates/runtime-apis");
    let staging = root.join("crates/.runtime-apis-generated");
    let backup = root.join("crates/.runtime-apis-backup");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).into_diagnostic()?;
    }
    miette::ensure!(
        !backup.exists(),
        "refusing to overwrite retained builtin generation backup {}",
        backup.display()
    );
    trellis_codegen_rust::generate_rust_package(&graph, &staging, "trellis-runtime-apis")
        .into_diagnostic()?;
    rustfmt_generated(&staging)?;
    if target.exists() {
        std::fs::rename(&target, &backup).into_diagnostic()?;
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        if backup.exists() {
            std::fs::rename(&backup, &target).into_diagnostic()?;
        }
        return Err(error).into_diagnostic();
    }
    if backup.exists() {
        std::fs::remove_dir_all(backup).into_diagnostic()?;
    }
    let target = root.join("ts/packages/trellis/internal_sdk/generated");
    let staging = root.join("ts/packages/trellis/internal_sdk/.generated-staging");
    let backup = root.join("ts/packages/trellis/internal_sdk/.generated-backup");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).into_diagnostic()?;
    }
    miette::ensure!(
        !backup.exists(),
        "refusing to overwrite retained builtin TypeScript generation backup {}",
        backup.display()
    );
    trellis_codegen_ts::generate_ts_package(&graph, &staging, "@oatscenter/trellis-internal-sdk")
        .into_diagnostic()?;
    if target.exists() {
        std::fs::rename(&target, &backup).into_diagnostic()?;
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        if backup.exists() {
            std::fs::rename(&backup, &target).into_diagnostic()?;
        }
        return Err(error).into_diagnostic();
    }
    if backup.exists() {
        std::fs::remove_dir_all(backup).into_diagnostic()?;
    }
    let builtin_semantics_path = root.join("crates/runtime/src/platform/auth/builtin_semantics.rs");
    std::fs::write(&builtin_semantics_path, builtin_semantics).into_diagnostic()?;
    rustfmt_generated(&builtin_semantics_path)?;
    Ok(())
}

fn project_testkit_admin_source(root: &Path) -> Result<()> {
    let source = root.join("crates/runtime-apis/src");
    let target = root.join("crates/trellis-test/src/runtime_api");
    let staging = root.join("crates/.trellis-test-runtime-api");
    let backup = root.join("crates/.trellis-test-runtime-api-backup");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).into_diagnostic()?;
    }
    miette::ensure!(
        !backup.exists(),
        "refusing to overwrite retained testkit projection backup {}",
        backup.display()
    );
    copy_source_tree(&source, &staging)?;
    if target.exists() {
        std::fs::rename(&target, &backup).into_diagnostic()?;
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        if backup.exists() {
            std::fs::rename(&backup, &target).into_diagnostic()?;
        }
        return Err(error).into_diagnostic();
    }
    if backup.exists() {
        std::fs::remove_dir_all(backup).into_diagnostic()?;
    }
    Ok(())
}

/// Copies generated Rust source byte-for-byte, skipping manifests and build output.
fn copy_source_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination).into_diagnostic()?;
    for entry in std::fs::read_dir(source).into_diagnostic()? {
        let entry = entry.into_diagnostic()?;
        let name = entry.file_name();
        if name == "Cargo.toml" || name == "Cargo.lock" || name == "target" {
            continue;
        }
        let path = entry.path();
        let destination_path = destination.join(&name);
        if path.is_dir() {
            copy_source_tree(&path, &destination_path)?;
        } else {
            std::fs::copy(&path, &destination_path).into_diagnostic()?;
        }
    }
    Ok(())
}

fn rustfmt_generated(path: &Path) -> Result<()> {
    let mut pending = vec![path.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                std::fs::read_dir(path)
                    .into_diagnostic()?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<Vec<_>>>()
                    .into_diagnostic()?,
            );
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    let status = Command::new("rustfmt")
        .args(["--edition", "2021"])
        .args(files)
        .status()
        .into_diagnostic()?;
    miette::ensure!(status.success(), "rustfmt failed for generated Rust");
    Ok(())
}

fn build_embedded_browser_apps() -> Result<()> {
    let root = repo_root()?;
    let status = Command::new("deno")
        .current_dir(root)
        .args(["task", "-c", "ts/deno.json", "browser:embedded"])
        .status()
        .into_diagnostic()
        .wrap_err("failed to build embedded browser applications")?;

    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "embedded browser application build failed with {status}"
        ))
    }
}

fn generate_protocol_wasm() -> Result<()> {
    let root = repo_root()?;
    let rust = root.clone();
    let status = Command::new("cargo")
        .current_dir(&rust)
        .args([
            "build",
            "-p",
            "trellis-protocol-wasm",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()
        .into_diagnostic()
        .wrap_err("failed to build protocol WASM")?;
    if !status.success() {
        return Err(miette::miette!("protocol WASM build failed with {status}"));
    }
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                rust.join(path)
            }
        })
        .unwrap_or_else(|| rust.join("target"));
    let input = target.join("wasm32-unknown-unknown/release/trellis_protocol_wasm.wasm");
    let output = root.join("ts/packages/trellis/auth/protocol_wasm");
    std::fs::create_dir_all(&output).into_diagnostic()?;
    let mut bindgen = wasm_bindgen_cli_support::Bindgen::new();
    bindgen
        .input_path(input)
        .web(true)
        .map_err(|error| miette::miette!(error.to_string()))?
        .typescript(true)
        .omit_default_module_path(true)
        .out_name("trellis_protocol_wasm")
        .generate(&output)
        .map_err(|error| miette::miette!(error.to_string()))?;
    let wasm = std::fs::read(output.join("trellis_protocol_wasm_bg.wasm")).into_diagnostic()?;
    std::fs::write(
        output.join("trellis_protocol_wasm_bytes.ts"),
        format!(
            "// Generated by cargo xtask protocol-wasm.\nexport const PROTOCOL_WASM_BASE64 = \"{}\";\n",
            base64(&wasm)
        ),
    )
    .into_diagnostic()?;
    Ok(())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        encoded.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn run_build(args: &[String]) -> Result<()> {
    run_install(false)?;
    generate_protocol_wasm()?;
    build_embedded_browser_apps()?;
    let workspace_root = repo_root()?;
    let mut spec = Command::new("cargo");
    spec.current_dir(&workspace_root).arg("build");
    for arg in args {
        spec.arg(arg);
    }
    let status = spec
        .status()
        .into_diagnostic()
        .wrap_err("failed to run cargo for build workflow")?;

    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "build workflow failed with status {status}"
        ))
    }
}

fn repo_root() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        if ancestor.join("crates/idl/Cargo.toml").exists() && ancestor.join("ts/deno.json").exists()
        {
            return Ok(ancestor.to_path_buf());
        }
    }
    Err(miette::miette!(
        "failed to resolve repository root from xtask manifest"
    ))
}

#[cfg(test)]
mod tests {
    use crate::release::ReleaseCommand;

    use super::{parse_command, XtaskCommand};

    #[test]
    fn parse_install_command() {
        let command = parse_command(["install".to_string()].into_iter())
            .expect("parse install")
            .expect("install command");
        assert_eq!(command, XtaskCommand::Install { update: false });
        let command = parse_command(["install", "--update"].into_iter().map(str::to_string))
            .expect("parse install update")
            .expect("install update command");
        assert_eq!(command, XtaskCommand::Install { update: true });
    }

    #[test]
    fn parse_protocol_wasm_command() {
        let command = parse_command(["protocol-wasm".to_string()].into_iter())
            .expect("parse protocol-wasm")
            .expect("protocol-wasm command");
        assert_eq!(command, XtaskCommand::ProtocolWasm);
    }

    #[test]
    fn parse_build_command_preserves_passthrough_args() {
        let command = parse_command(
            ["build", "--workspace", "--release"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("parse build")
        .expect("build command");
        assert_eq!(
            command,
            XtaskCommand::Build {
                args: vec!["--workspace".to_string(), "--release".to_string()],
            }
        );
    }

    #[test]
    fn parse_build_command_preserves_help_passthrough_arg() {
        let command = parse_command(["build", "--help"].into_iter().map(str::to_string))
            .expect("parse build help argument")
            .expect("build command");
        assert_eq!(
            command,
            XtaskCommand::Build {
                args: vec!["--help".to_string()],
            }
        );
    }

    #[test]
    fn parse_release_command() {
        let command = parse_command(
            ["release", "check-versions"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("parse release command")
        .expect("release command");
        assert_eq!(
            command,
            XtaskCommand::Release {
                command: ReleaseCommand::CheckVersions,
            }
        );
    }

    #[test]
    fn parse_help_command_succeeds_without_exiting() {
        let command = parse_command(["--help"].into_iter().map(str::to_string))
            .expect("help should be handled successfully");
        assert!(command.is_none());
    }

    #[test]
    fn parse_build_command_preserves_argument_delimiter() {
        let command = parse_command(
            ["build", "--", "--cfg", "foo"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("parse build")
        .expect("build command");
        assert_eq!(
            command,
            XtaskCommand::Build {
                args: vec!["--", "--cfg", "foo"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            }
        );
    }

    #[test]
    fn install_rejects_extra_args() {
        let error = parse_command(["install", "--workspace"].into_iter().map(str::to_string))
            .expect_err("install should reject extra args");
        assert!(error.to_string().contains("unexpected argument"));
    }

    #[test]
    fn protocol_wasm_rejects_extra_args() {
        let error = parse_command(
            ["protocol-wasm", "--workspace"]
                .into_iter()
                .map(str::to_string),
        )
        .expect_err("protocol-wasm should reject extra args");
        assert!(error.to_string().contains("unexpected argument"));
    }
}
