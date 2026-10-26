//! Editing a project's own IDL needs only `trellis generate`: generation
//! re-records the project's package in `trellis.lock`, while `--check` reports
//! the stale lock instead of rewriting it.

use std::fs;
use std::path::Path;

use trellis_cli::{
    cli::{GenerateArgs, OutputFormat, ProjectRootArgs},
    generate, package,
};

const MANIFEST: &str = "[package]\nname = \"orders\"\nversion = \"0.1.0\"\n\n[sources]\n\
    doc = \"orders.trellis\"\n\n[generate.typescript]\noutput = \"generated\"\n";

fn source(rpc: &str) -> String {
    format!(
        "model Value {{ value: string; }}\napi orders@v1 {{\n  title \"Orders\";\n  \
        description \"Orders\";\n  rpc {rpc} {{ input Value; output Value; }}\n  \
        capabilities {{ public {{ allows {{ rpc {rpc}; }} }} }}\n}}\n\
        service OrdersService {{ implements orders; }}\n"
    )
}

fn run(root: &Path, check: bool) -> miette::Result<()> {
    generate::run(&GenerateArgs {
        watch: false,
        check,
        project: ProjectRootArgs {
            root: root.to_path_buf(),
        },
    })
}

#[tokio::test]
async fn an_idl_edit_needs_only_generate() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("trellis.toml"), MANIFEST).unwrap();
    fs::write(root.path().join("orders.trellis"), source("Get")).unwrap();
    package::install(
        OutputFormat::Text,
        &ProjectRootArgs {
            root: root.path().to_path_buf(),
        },
    )
    .await
    .expect("install locks the project");
    let locked = fs::read_to_string(root.path().join("trellis.lock")).unwrap();

    fs::write(root.path().join("orders.trellis"), source("Find")).unwrap();
    let stale = run(root.path(), true).expect_err("--check reports the stale lock");
    assert!(
        stale.to_string().contains("differs from trellis.lock"),
        "{stale}"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("trellis.lock")).unwrap(),
        locked,
        "--check leaves the lock alone"
    );

    run(root.path(), false).expect("generate re-records the edited package");
    assert_ne!(
        fs::read_to_string(root.path().join("trellis.lock")).unwrap(),
        locked
    );
    run(root.path(), true).expect("the regenerated project is current");
    let generated = fs::read_to_string(root.path().join("generated/apis/orders/mod.js")).unwrap();
    assert!(
        generated.contains("Find"),
        "generation used the edited source"
    );
}
