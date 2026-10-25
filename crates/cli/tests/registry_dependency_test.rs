//! A project depends on the published `trellis` package from its OCI registry:
//! install pulls and compiles the frozen registry package, and generation
//! consumes it. This uses the real public GHCR package, so it needs network.

use std::fs;

use trellis_cli::{
    cli::{GenerateArgs, OutputFormat, ProjectRootArgs},
    generate, package,
};

const MANIFEST: &str = r#"[package]
name = "consumer"
version = "0.1.0"

[sources]
doc = "consumer.trellis"

[dependencies.trellis]
package = "trellis"
version = "0.100.0-rc.4"
registry = "oats"

[registries.oats]
prefix = "ghcr.io/oats-center"

[generate.typescript]
output = "generated"
"#;

const SOURCE: &str = "import { health } from trellis;\n\nmodel C { value: string; }\n\n\
api consumer@v1 {\n  title \"Consumer\";\n  description \"Registry consumer\";\n  \
rpc Get { input C; output C; }\n  capabilities { public { allows { rpc Get; } } }\n}\n\n\
service ConsumerService {\n  implements consumer;\n  \
use health { required capability health.observe; }\n}\n";

#[tokio::test]
async fn a_project_installs_and_generates_against_a_published_registry_package() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // This test binary holds only this test, so the process environment is ours.
    unsafe { std::env::set_var("TRELLIS_CACHE", cache.path()) };
    fs::write(root.path().join("trellis.toml"), MANIFEST).unwrap();
    fs::write(root.path().join("consumer.trellis"), SOURCE).unwrap();
    let project = ProjectRootArgs {
        root: root.path().to_path_buf(),
    };
    package::install(OutputFormat::Text, &project)
        .await
        .expect("install pulls and compiles the registry dependency");
    generate::run(&GenerateArgs {
        watch: false,
        check: false,
        project,
    })
    .expect("generation consumes the registry dependency");
    assert!(root.path().join("generated/apis/health").is_dir());
}
