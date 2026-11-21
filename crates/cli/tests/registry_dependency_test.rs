//! A project depends on the published built-in `trellis` package: install
//! locks it as the release at `ghcr.io/oats-center/trellis`, takes that release
//! from the CLI's own build of it instead of downloading it, and generation
//! consumes it.

use std::fs;

use trellis_cli::{
    cli::{GenerateArgs, OutputFormat, ProjectRootArgs},
    generate, package,
};
use trellis_idl::project::{read_lock, read_manifest, LockedSource};

const SOURCE: &str = "import { health } from trellis;\n\nmodel C { value: string; }\n\n\
api consumer@v1 {\n  title \"Consumer\";\n  description \"Registry consumer\";\n  \
rpc Get { input C; output C; }\n  capabilities { public { allows { rpc Get; } } }\n}\n\n\
service ConsumerService {\n  implements consumer;\n  \
use health { required capability health.observe; }\n}\n";

#[tokio::test]
async fn a_project_installs_and_generates_against_the_published_builtin_package() {
    let builtin = read_manifest(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../builtin-contracts/trellis.toml"),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // This test binary holds only this test, so the process environment is ours.
    unsafe { std::env::set_var("TRELLIS_CACHE", cache.path()) };
    fs::write(
        root.path().join("trellis.toml"),
        format!(
            "[package]\nname = \"consumer\"\nworkspace = \"ghcr.io/acme\"\nversion = \
            \"0.1.0\"\n\n[sources]\ndoc = \"consumer.trellis\"\n\n[dependencies.trellis]\n\
            package = \"trellis\"\nversion = \"={}\"\n\n[generate.typescript]\noutput = \
            \"generated\"\n",
            builtin.package.version
        ),
    )
    .unwrap();
    fs::write(root.path().join("consumer.trellis"), SOURCE).unwrap();
    let project = ProjectRootArgs {
        root: root.path().to_path_buf(),
    };
    package::install(OutputFormat::Text, &project)
        .await
        .expect("install resolves the built-in registry dependency");
    let lock = read_lock(&root.path().join("trellis.lock")).unwrap();
    let trellis = lock
        .packages
        .iter()
        .find(|package| package.id == "trellis")
        .unwrap();
    assert!(
        matches!(&trellis.source, LockedSource::Registry { workspace, .. } if workspace == "ghcr.io/oats-center"),
        "{:?}",
        trellis.source
    );
    generate::run(
        OutputFormat::Text,
        &GenerateArgs {
            watch: false,
            check: false,
            project,
        },
    )
    .expect("generation consumes the registry dependency");
    assert!(root.path().join("generated/apis/health").is_dir());
}
