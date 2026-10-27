use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result, WrapErr};

mod release;
mod rust_width;

#[derive(Debug, Clone, Eq, PartialEq, Subcommand)]
enum XtaskCommand {
    #[command(name = "install")]
    Install {
        #[arg(long)]
        update: bool,
        /// Skip rebuilding the protocol WASM, for callers that build it
        /// separately (such as CI's parallel WASM job).
        #[arg(long)]
        skip_protocol_wasm: bool,
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
    /// Fail when authored Rust has code lines rustfmt could not wrap.
    #[command(name = "rust-width")]
    RustWidth { files: Vec<std::path::PathBuf> },
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
        XtaskCommand::Install {
            update,
            skip_protocol_wasm,
        } => run_install(update, !skip_protocol_wasm),
        XtaskCommand::ProtocolWasm => generate_protocol_wasm(),
        XtaskCommand::Build { args } => run_build(&args),
        XtaskCommand::Release { command } => release::run_release(&repo_root()?, command),
        XtaskCommand::RustWidth { files } => rust_width::run(&repo_root()?, files),
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
    "crates/builtin-contracts",
    "ts/packages/trellis-testkit",
    "web",
    "integration/fixtures/runtime",
    "integration/fixtures/runtime/v2",
    "integration/fixtures/runtime-removed",
    "integration/fixtures/client-upgrade",
    "integration/fixtures/client-upgrade-narrow",
    "integration/fixtures/client-upgrade-consent",
    "integration/fixtures/client-upgrade-breaking",
    "integration/fixtures/runtime-pinned",
    "integration/fixtures/testkit",
    "benchmarks/runtime",
    "demos/ts/service",
    "demos/ts/device",
    "demos/app",
    "demos/rust/service",
    "demos/rust/device",
    "docs/examples/orders",
];

/// Installs contract dependencies and regenerates every generated artifact the
/// SDKs consume, including the protocol WASM unless `protocol_wasm` is false.
fn run_install(update: bool, protocol_wasm: bool) -> Result<()> {
    let root = repo_root()?;
    let runtime = tokio::runtime::Runtime::new().into_diagnostic()?;
    generate_builtin_package(&root).wrap_err("generating built-in package")?;
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
        root.join("ts/packages/trellis-testkit/src/nats-binaries.json"),
    )
    .into_diagnostic()?;
    if protocol_wasm {
        generate_protocol_wasm()?;
    }
    Ok(())
}

fn generate_builtin_package(root: &Path) -> Result<()> {
    let graph = trellis_builtin_contracts::compile()?;
    let sdk = root.join("crates/trellis");
    let sdk_target = sdk.join("generated");
    const HEADER: &str = "// Generated by cargo xtask install. Do not edit.\n";
    if sdk_target.exists() {
        for entry in std::fs::read_dir(&sdk_target).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            miette::ensure!(
                entry.file_type().into_diagnostic()?.is_file()
                    && std::fs::read(entry.path())
                        .into_diagnostic()?
                        .starts_with(HEADER.as_bytes()),
                "refusing to replace non-generated SDK input {}",
                entry.path().display()
            );
        }
    }
    let staged = tempfile::Builder::new()
        .prefix(".generated-staging-")
        .tempdir_in(&sdk)
        .into_diagnostic()?;
    let fresh = staged.path().join("new");
    std::fs::create_dir(&fresh).into_diagnostic()?;
    let model_binding = trellis_codegen_rust::RustModelBinding::from_package_manifest(
        &graph,
        include_str!("../../crates/models/Cargo.toml"),
    )
    .into_diagnostic()?;
    let aliases =
        trellis_codegen_rust::render_rust_models(&graph, &[model_binding]).into_diagnostic()?;
    std::fs::write(fresh.join("__types.rs"), format!("{HEADER}{aliases}")).into_diagnostic()?;
    let models = root.join("crates/models");
    let models_target = models.join("generated");
    if models_target.exists() {
        for entry in std::fs::read_dir(&models_target).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            let bytes = std::fs::read(entry.path()).into_diagnostic()?;
            // Earlier installers also wrote this model snapshot; it is
            // generated output the replacement retires, not an authored input.
            let retired_snapshot = entry.file_name() == "models.json"
                && serde_json::from_slice::<serde_json::Value>(&bytes)
                    .is_ok_and(|snapshot| snapshot["format"] == "trellis.rust-models.v1");
            miette::ensure!(
                entry.file_type().into_diagnostic()?.is_file()
                    && (bytes.starts_with(HEADER.as_bytes()) || retired_snapshot),
                "refusing to replace non-generated model input {}",
                entry.path().display()
            );
        }
    }
    let model_stage = tempfile::Builder::new()
        .prefix(".generated-staging-")
        .tempdir_in(&models)
        .into_diagnostic()?;
    let model_fresh = model_stage.path().join("new");
    std::fs::create_dir(&model_fresh).into_diagnostic()?;
    let types = trellis_codegen_rust::render_rust_models(&graph, &[]).into_diagnostic()?;
    std::fs::write(model_fresh.join("__types.rs"), format!("{HEADER}{types}")).into_diagnostic()?;
    let mut modules = String::from("#[doc(hidden)] #[path = \"__types.rs\"] pub mod __types;\n");
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
        let source = trellis_codegen_rust::render_runtime_action(&graph, api, kind, action)
            .into_diagnostic()?;
        std::fs::write(
            fresh.join(format!("{module}.rs")),
            format!("{HEADER}{source}"),
        )
        .into_diagnostic()?;
        modules.push_str(&format!(
            "#[doc(hidden)] #[path = \"{module}.rs\"] {visibility}mod {module};\n"
        ));
    }
    std::fs::write(fresh.join("modules.rs"), format!("{HEADER}{modules}")).into_diagnostic()?;
    let testkit = root.join("crates/trellis-testkit");
    let (testkit_stage, testkit_fresh, testkit_target) =
        stage_bindings(&graph, &testkit, "trellis-testkit-bindings")?;
    std::fs::copy(
        root.join("crates/local-nats/nats-binaries.json"),
        testkit_fresh.join("nats-binaries.json"),
    )
    .into_diagnostic()?;
    std::fs::write(
        testkit_fresh.join("modules.rs"),
        "#[doc(hidden)] #[path = \"src/lib.rs\"] pub mod __bindings;\nuse __bindings::*;\n",
    )
    .into_diagnostic()?;
    ensure_generated(&testkit_fresh, &testkit_target)?;
    let runtime_apis = root.join("crates/runtime-apis");
    let (runtime_apis_stage, runtime_apis_fresh, runtime_apis_target) =
        stage_bindings(&graph, &runtime_apis, "trellis-runtime-apis")?;
    std::fs::write(
        runtime_apis_fresh.join("modules.rs"),
        "#[path = \"src/lib.rs\"] mod generated;\npub use generated::*;\n",
    )
    .into_diagnostic()?;
    ensure_generated(&runtime_apis_fresh, &runtime_apis_target)?;
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
    publish_generated_directories(&[
        (
            &model_fresh,
            &models_target,
            &models.join(".generated-backup"),
        ),
        (&fresh, &sdk_target, &sdk.join(".generated-backup")),
        (
            &testkit_fresh,
            &testkit_target,
            &testkit.join(".generated-backup"),
        ),
        (
            &runtime_apis_fresh,
            &runtime_apis_target,
            &runtime_apis.join(".generated-backup"),
        ),
        (&staging, &target, &backup),
    ])?;
    drop((testkit_stage, runtime_apis_stage));
    Ok(())
}

/// Refuses to replace a `generated` directory holding anything that is neither
/// in the staged replacement nor marked as generated.
fn ensure_generated(fresh: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        miette::ensure!(
            std::fs::symlink_metadata(target)
                .into_diagnostic()?
                .is_dir(),
            "refusing to replace non-directory output {}",
            target.display()
        );
        let mut directories = vec![target.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).into_diagnostic()? {
                let entry = entry.into_diagnostic()?;
                let kind = entry.file_type().into_diagnostic()?;
                if kind.is_dir() {
                    directories.push(entry.path());
                    continue;
                }
                let path = entry.path();
                let relative = path.strip_prefix(target).into_diagnostic()?;
                let generated = fresh.join(relative).is_file()
                    || (kind.is_file() && {
                        let bytes = std::fs::read(&path).into_diagnostic()?;
                        bytes.starts_with(b"//! Generated") || bytes.starts_with(b"// Generated")
                    });
                miette::ensure!(
                    kind.is_file() && generated,
                    "refusing to replace non-generated input {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

/// Generates the built-in API bindings a crate includes from its `generated`
/// directory, staged beside it. Returns the staging guard, the staged output,
/// and the target.
fn stage_bindings(
    graph: &trellis_idl::PackageGraph,
    crate_dir: &Path,
    package_name: &str,
) -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
    let stage = tempfile::Builder::new()
        .prefix(".generated-staging-")
        .tempdir_in(crate_dir)
        .into_diagnostic()?;
    let fresh = stage.path().join("new");
    trellis_codegen_rust::generate_rust_package(
        graph,
        &fresh,
        package_name,
        &[
            trellis_codegen_rust::RustModelBinding::from_package_manifest(
                graph,
                include_str!("../../crates/models/Cargo.toml"),
            )
            .into_diagnostic()?,
        ],
    )
    .into_diagnostic()?;
    // These bindings are package-relative modules, not a nested Cargo package.
    // Cargo excludes nested packages even when their sources are included explicitly.
    std::fs::remove_file(fresh.join("Cargo.toml")).into_diagnostic()?;
    let target = crate_dir.join("generated");
    Ok((stage, fresh, target))
}

fn publish_generated_directories(outputs: &[(&Path, &Path, &Path)]) -> Result<()> {
    for (_, _, backup) in outputs {
        miette::ensure!(
            !backup.exists(),
            "refusing to overwrite retained generation backup {}",
            backup.display()
        );
    }
    for (index, (staged, target, backup)) in outputs.iter().enumerate() {
        let result = (|| -> std::io::Result<()> {
            if target.exists() {
                std::fs::rename(target, backup)?;
            }
            std::fs::rename(staged, target)
        })();
        if let Err(error) = result {
            let mut recovery_errors = Vec::new();
            if backup.exists() && !target.exists() {
                if let Err(error) = std::fs::rename(backup, target) {
                    recovery_errors.push(error.to_string());
                }
            }
            for (staged, target, backup) in outputs[..index].iter().rev() {
                if let Err(error) = std::fs::rename(target, staged) {
                    recovery_errors.push(error.to_string());
                    continue;
                }
                if backup.exists() {
                    if let Err(error) = std::fs::rename(backup, target) {
                        recovery_errors.push(error.to_string());
                    }
                }
            }
            return Err(miette::miette!(
                "generated output publication failed: {error}; restoration errors: \
                {recovery_errors:?}; any unrestored last-good output remains in its \
                backup"
            ));
        }
    }
    for (_, _, backup) in outputs {
        if backup.exists() {
            std::fs::remove_dir_all(backup).into_diagnostic()?;
        }
    }
    Ok(())
}

fn build_embedded_browser_apps() -> Result<()> {
    let root = repo_root()?;
    let status = Command::new("npm")
        .current_dir(root)
        .args(["run", "browser:embedded"])
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
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join("target"));
    let output = root.join("ts/packages/trellis/auth/protocol_wasm");
    // Callers load `client`; services and devices load `provider`, a superset
    // with the exports only request-serving participants need.
    for (variant, features) in [("client", None), ("provider", Some("provider"))] {
        // The two feature sets link to the same cdylib name, so a shared
        // target directory would relink both on every install.
        let variant_target = target.join(format!("protocol-wasm-{variant}"));
        let mut build = Command::new("cargo");
        build.current_dir(&root).args([
            "build",
            "-p",
            "trellis-protocol-wasm",
            "--target",
            "wasm32-unknown-unknown",
            "--profile",
            "protocol-wasm",
        ]);
        build.arg("--target-dir").arg(&variant_target);
        if let Some(features) = features {
            build.args(["--features", features]);
        }
        let status = build
            .status()
            .into_diagnostic()
            .wrap_err("failed to build protocol WASM")?;
        if !status.success() {
            return Err(miette::miette!(
                "protocol WASM {variant} build failed with {status}"
            ));
        }
        let input =
            variant_target.join("wasm32-unknown-unknown/protocol-wasm/trellis_protocol_wasm.wasm");
        let directory = output.join(variant);
        // Skip bindings and size optimization when this exact module and this
        // generator already produced the output.
        let stamp = {
            use sha2::Digest as _;
            let mut hasher = sha2::Sha256::new();
            hasher.update(std::fs::read(&input).into_diagnostic()?);
            hasher.update(include_bytes!("main.rs"));
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let stamp_path = directory.join(".input-sha256");
        if std::fs::read_to_string(&stamp_path).is_ok_and(|current| current == stamp) {
            continue;
        }
        if directory.exists() {
            std::fs::remove_dir_all(&directory).into_diagnostic()?;
        }
        std::fs::create_dir_all(&directory).into_diagnostic()?;
        let mut bindgen = wasm_bindgen_cli_support::Bindgen::new();
        bindgen
            .input_path(input)
            .web(true)
            .map_err(|error| miette::miette!(error.to_string()))?
            .typescript(true)
            .omit_default_module_path(true)
            .out_name("trellis_protocol_wasm")
            .generate(&directory)
            .map_err(|error| miette::miette!(error.to_string()))?;
        let wasm = directory.join("trellis_protocol_wasm_bg.wasm");
        // Exactly the features rustc emits for wasm32-unknown-unknown, so the
        // optimizer never introduces an instruction a runtime may lack.
        wasm_opt::OptimizationOptions::new_optimize_for_size()
            .enable_feature(wasm_opt::Feature::BulkMemory)
            .enable_feature(wasm_opt::Feature::Multivalue)
            .enable_feature(wasm_opt::Feature::MutableGlobals)
            .enable_feature(wasm_opt::Feature::TruncSat)
            .enable_feature(wasm_opt::Feature::ReferenceTypes)
            .enable_feature(wasm_opt::Feature::SignExt)
            .run(&wasm, &wasm)
            .map_err(|error| miette::miette!("wasm-opt {variant}: {error}"))?;
        std::fs::write(&stamp_path, stamp).into_diagnostic()?;
    }
    Ok(())
}

fn run_build(args: &[String]) -> Result<()> {
    run_install(false, true)?;
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
        if ancestor.join("crates/idl/Cargo.toml").exists() && ancestor.join("package.json").exists()
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
    fn failed_publication_restores_model_and_sdk_outputs_and_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let model = root.path().join("models");
        let sdk = root.path().join("sdk");
        let fresh_model = root.path().join("fresh-models");
        let fresh_sdk = root.path().join("fresh-sdk");
        let model_backup = root.path().join("models-backup");
        let sdk_backup = root.path().join("sdk-backup");
        for (directory, contents) in [
            (&model, "old-model"),
            (&sdk, "old-sdk"),
            (&fresh_model, "new-model"),
        ] {
            std::fs::create_dir(directory).unwrap();
            std::fs::write(directory.join("data"), contents).unwrap();
        }
        let outputs = [
            (
                fresh_model.as_path(),
                model.as_path(),
                model_backup.as_path(),
            ),
            (fresh_sdk.as_path(), sdk.as_path(), sdk_backup.as_path()),
        ];
        assert!(super::publish_generated_directories(&outputs).is_err());
        assert_eq!(
            std::fs::read_to_string(model.join("data")).unwrap(),
            "old-model"
        );
        assert_eq!(
            std::fs::read_to_string(sdk.join("data")).unwrap(),
            "old-sdk"
        );
        std::fs::create_dir(&fresh_sdk).unwrap();
        std::fs::write(fresh_sdk.join("data"), "new-sdk").unwrap();
        super::publish_generated_directories(&outputs).unwrap();
        assert_eq!(
            std::fs::read_to_string(model.join("data")).unwrap(),
            "new-model"
        );
        assert_eq!(
            std::fs::read_to_string(sdk.join("data")).unwrap(),
            "new-sdk"
        );
    }

    #[test]
    fn parse_install_command() {
        let command = parse_command(["install".to_string()].into_iter())
            .expect("parse install")
            .expect("install command");
        assert_eq!(
            command,
            XtaskCommand::Install {
                update: false,
                skip_protocol_wasm: false
            }
        );
        let command = parse_command(["install", "--update"].into_iter().map(str::to_string))
            .expect("parse install update")
            .expect("install update command");
        assert_eq!(
            command,
            XtaskCommand::Install {
                update: true,
                skip_protocol_wasm: false
            }
        );
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
