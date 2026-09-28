//! Functional regression: the same local path-dependency reached directly and
//! transitively through a sibling package must resolve to one exact source.
//!
//! These are real filesystem tests: they install (resolve + lock + generate) a
//! real temporary package graph. They assert dependency identity, not generated
//! output shape.

use std::fs;
use std::path::Path;

use trellis_cli::{
    cli::{GenerateArgs, OutputFormat, ProjectRootArgs},
    generate, package,
};
use trellis_idl::project::{LockedSource, PackageLock};

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn manifest(name: &str, source: &str, dependencies: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[sources]\ndoc = \"{source}\"\n\n{dependencies}[generate.typescript]\noutput = \"generated\"\n"
    )
}

const A_SOURCE: &str = "model A {}\napi a@v1 {\n  title \"A\";\n  description \"A\";\n  rpc Get { input A; output A; }\n  capabilities { public { allows { rpc Get; } } }\n}\nservice AService { implements a; }\n";
const B_SOURCE: &str = "import { a } from aye;\nmodel B {}\napi b@v1 {\n  title \"B\";\n  description \"B\";\n  rpc Get { input B; output B; }\n  capabilities { public { allows { rpc Get; } } }\n}\nservice BService { implements b; use a { rpc Get; } }\n";
const C_SOURCE: &str = "import { a } from a;\nimport { b } from b;\nmodel C {}\napi c@v1 {\n  title \"C\";\n  description \"C\";\n  rpc Get { input C; output C; }\n  capabilities { public { allows { rpc Get; } } }\n}\nservice CService { implements c; use a { rpc Get; } use b { rpc Get; } }\n";

async fn install_and_generate(root: &Path) {
    let project = ProjectRootArgs {
        root: root.to_path_buf(),
    };
    package::install(OutputFormat::Text, &project)
        .await
        .expect("install resolves the local dependency graph");
    generate::run(&GenerateArgs {
        watch: false,
        check: true,
        project,
    })
    .expect("generation succeeds for the resolved graph");
}

fn source_path(lock: &PackageLock, name: &str) -> String {
    let package = lock
        .packages
        .iter()
        .find(|package| package.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name} is absent from the lock"));
    match &package.source {
        LockedSource::Path { path } => path.clone(),
        LockedSource::Registry { .. } => panic!("{name} resolved to a registry source"),
    }
}

fn digest(lock: &PackageLock, name: &str) -> String {
    lock.packages
        .iter()
        .find(|package| package.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name} is absent from the lock"))
        .digest
        .clone()
}

fn assert_dependency(lock: &PackageLock, owner: &str, dependency: &str) {
    let package = lock
        .packages
        .iter()
        .find(|package| package.name.as_str() == owner)
        .unwrap_or_else(|| panic!("{owner} is absent from the lock"));
    let edge = package
        .dependencies
        .iter()
        .find(|edge| edge.name.as_str() == dependency)
        .unwrap_or_else(|| panic!("{owner} does not depend on {dependency}"));
    assert_eq!(
        edge.digest,
        digest(lock, dependency),
        "{owner} must depend on the exact {dependency} source"
    );
}

/// One local package reached directly and through a sibling records a single
/// exact source, and both dependency edges agree on its digest.
#[tokio::test]
async fn diamond_local_dependency_resolves_one_exact_source() {
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join("trellis.toml"),
        &manifest(
            "pkg-c",
            "c.trellis",
            "[dependencies]\na = { package = \"pkg-a\", path = \"a\" }\nb = { package = \"pkg-b\", path = \"b\" }\n",
        ),
    );
    write(&root.path().join("c.trellis"), C_SOURCE);
    write(
        &root.path().join("a/trellis.toml"),
        &manifest("pkg-a", "a.trellis", ""),
    );
    write(&root.path().join("a/a.trellis"), A_SOURCE);
    write(
        &root.path().join("b/trellis.toml"),
        &manifest(
            "pkg-b",
            "b.trellis",
            "[dependencies]\naye = { package = \"pkg-a\", path = \"../a\" }\n",
        ),
    );
    write(&root.path().join("b/b.trellis"), B_SOURCE);

    install_and_generate(root.path()).await;

    let lock = trellis_idl::project::read_lock(&root.path().join("trellis.lock")).unwrap();
    assert_eq!(
        lock.packages
            .iter()
            .filter(|package| package.name.as_str() == "pkg-a")
            .count(),
        1,
        "the shared dependency appears exactly once"
    );
    assert_eq!(source_path(&lock, "pkg-a"), "a");
    assert_eq!(source_path(&lock, "pkg-b"), "b");
    assert_dependency(&lock, "pkg-b", "pkg-a");
    assert_dependency(&lock, "pkg-c", "pkg-a");
    assert_dependency(&lock, "pkg-c", "pkg-b");
    assert!(root.path().join("generated/index.js").is_file());
}

/// Parent traversal through a symlinked directory must record the canonical
/// dependency location, not the lexically collapsed spelling (which points at a
/// different directory).
#[cfg(unix)]
#[tokio::test]
async fn symlinked_parent_traversal_records_canonical_source() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("deep/inner")).unwrap();
    std::os::unix::fs::symlink(root.path().join("deep/inner"), root.path().join("link")).unwrap();
    write(
        &root.path().join("trellis.toml"),
        &manifest(
            "pkg-c",
            "c.trellis",
            "[dependencies]\na = { package = \"pkg-a\", path = \"deep/a\" }\nb = { package = \"pkg-b\", path = \"b\" }\n",
        ),
    );
    write(&root.path().join("c.trellis"), C_SOURCE);
    write(
        &root.path().join("deep/a/trellis.toml"),
        &manifest("pkg-a", "a.trellis", ""),
    );
    write(&root.path().join("deep/a/a.trellis"), A_SOURCE);
    write(
        &root.path().join("b/trellis.toml"),
        &manifest(
            "pkg-b",
            "b.trellis",
            "[dependencies]\naye = { package = \"pkg-a\", path = \"../link/../a\" }\n",
        ),
    );
    write(&root.path().join("b/b.trellis"), B_SOURCE);

    install_and_generate(root.path()).await;

    let lock = trellis_idl::project::read_lock(&root.path().join("trellis.lock")).unwrap();
    assert_eq!(source_path(&lock, "pkg-a"), "deep/a");
    assert_dependency(&lock, "pkg-b", "pkg-a");
    assert_dependency(&lock, "pkg-c", "pkg-a");
    assert_dependency(&lock, "pkg-c", "pkg-b");
    assert!(root.path().join("generated/index.js").is_file());
}
