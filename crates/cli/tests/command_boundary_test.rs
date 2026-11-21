use std::{fs, process::Command};

/// Two packages named `orders` from different workspaces stay separate
/// dependencies: a second `add` under the alias the first holds is refused
/// and changes nothing, `--alias` keeps both, and `rm` removes only the one
/// its argument names, refusing a bare name that fits both.
#[test]
fn same_named_dependencies_from_two_workspaces_stay_apart() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    for (directory, workspace) in [("alpha", "ghcr.io/alpha"), ("beta", "ghcr.io/beta")] {
        fs::create_dir_all(root.join(directory)).unwrap();
        fs::write(
            root.join(directory).join("trellis.toml"),
            format!(
                "[package]\nname = \"orders\"\nworkspace = \"{workspace}\"\nversion = \
                \"1.0.0\"\n[sources]\nmain = \"main.trellis\"\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join(directory).join("main.trellis"),
            "model Order { id: string; }\n",
        )
        .unwrap();
    }
    fs::write(
        root.join("trellis.toml"),
        "[package]\nname = \"shop\"\nversion = \"1.0.0\"\n[sources]\nmain = \
        \"main.trellis\"\n",
    )
    .unwrap();
    fs::write(root.join("main.trellis"), "model Cart { id: string; }\n").unwrap();
    let trellis = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_trellis"))
            .args(args)
            .arg("--root")
            .arg(root)
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };
    let succeeds = |args: &[&str]| {
        let output = trellis(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let project_files = || {
        (
            fs::read(root.join("trellis.toml")).unwrap(),
            fs::read(root.join("trellis.lock")).unwrap(),
        )
    };
    // Each alias with the directory it names, read back from the manifest the
    // CLI wrote.
    let dependencies = || {
        let manifest = trellis_idl::project::read_manifest(&root.join("trellis.toml")).unwrap();
        manifest
            .dependencies
            .into_iter()
            .map(|(alias, dependency)| (alias, dependency.path.unwrap()))
            .collect::<Vec<_>>()
    };
    let locked = || {
        let lock = trellis_idl::project::read_lock(&root.join("trellis.lock")).unwrap();
        let mut ids = lock
            .packages
            .into_iter()
            .map(|package| package.id)
            .filter(|id| id != "localhost/shop")
            .collect::<Vec<_>>();
        ids.sort();
        ids
    };
    let refused = |args: &[&str]| {
        let before = project_files();
        let output = trellis(args);
        assert!(!output.status.success(), "{args:?} succeeded");
        assert_eq!(project_files(), before, "{args:?} changed the project");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    succeeds(&["add", "alpha"]);
    let error = refused(&["add", "beta"]);
    assert!(
        error.contains("ghcr.io/alpha/orders") && error.contains("--alias"),
        "{error}"
    );
    succeeds(&["add", "beta", "--alias", "beta"]);
    assert_eq!(
        dependencies(),
        [
            ("beta".into(), "beta".into()),
            ("orders".into(), "alpha".into())
        ]
    );
    assert_eq!(locked(), ["ghcr.io/alpha/orders", "ghcr.io/beta/orders"]);

    // `orders` is an exact alias, and only that dependency goes.
    succeeds(&["rm", "orders"]);
    assert_eq!(dependencies(), [("beta".into(), "beta".into())]);
    assert_eq!(locked(), ["ghcr.io/beta/orders"]);

    succeeds(&["add", "alpha", "--alias", "alpha"]);
    let error = refused(&["rm", "orders"]);
    assert!(
        error.contains("ghcr.io/alpha/orders") && error.contains("ghcr.io/beta/orders"),
        "{error}"
    );
    succeeds(&["rm", "ghcr.io/beta/orders"]);
    assert_eq!(dependencies(), [("alpha".into(), "alpha".into())]);
    assert_eq!(locked(), ["ghcr.io/alpha/orders"]);
}

#[test]
fn installed_project_checks_and_reports_invalid_authored_types() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("trellis.toml"),
        "[package]\nname = \"command-fixture\"\nversion = \
        \"1.0.0\"\n[sources]\nmain = \
        \"main.trellis\"\n[generate.typescript]\noutput = \"generated\"\n",
    )
    .unwrap();
    let source = "model Value { value: string; }\napi ping@v1 { title \"Ping\"; \
        description \"Command fixture\"; rpc Get { input Value; output Value; \
        } capabilities { public { allows { rpc Get; } } } \
        }\n";
    fs::write(root.path().join("main.trellis"), source).unwrap();

    for command in ["install", "check"] {
        let output = Command::new(env!("CARGO_BIN_EXE_trellis"))
            .args([command, "--root"])
            .arg(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::write(
        root.path().join("main.trellis"),
        source.replace("input Value", "input Missing"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_trellis"))
        .args(["check", "--root"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "invalid authored types must fail check"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Missing"));
}

/// A dumb terminal gets plain status lines even when `CI` is set, which
/// otherwise turns color on for any terminal; a capable terminal under `CI`
/// still gets color.
#[test]
fn dumb_terminal_gets_no_escape_codes_under_ci() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("trellis.toml"),
        "[package]\nname = \"color-fixture\"\nversion = \"1.0.0\"\n[sources]\nmain = \
        \"main.trellis\"\n",
    )
    .unwrap();
    fs::write(
        root.path().join("main.trellis"),
        "model Value { value: string; }\n",
    )
    .unwrap();
    let on_terminal = |term: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_trellis"));
        command
            .args(["check", "--root"])
            .arg(root.path())
            .env("CI", "true")
            .env("TERM", term)
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR")
            .env_remove("CLICOLOR_FORCE");
        let mut session = rexpect::session::spawn_command(command, Some(30_000)).unwrap();
        session.exp_eof().unwrap()
    };
    let colored = on_terminal("xterm-256color");
    assert!(
        colored.contains("is valid") && colored.contains('\x1b'),
        "{colored:?}"
    );
    let plain = on_terminal("dumb");
    assert!(plain.contains("is valid"), "{plain:?}");
    assert!(!plain.contains('\x1b'), "{plain:?}");
}
