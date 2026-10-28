use super::*;
use clap::Parser;

#[test]
fn parses_check_and_dependency_specific_update_commands() {
    let cli = Cli::parse_from(["trellis", "check", "--root", "project"]);
    match cli.command {
        TopLevelCommand::Check(args) => assert_eq!(args.root, PathBuf::from("project")),
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "update", "common", "--root", "project"]);
    match cli.command {
        TopLevelCommand::Update(args) => {
            assert_eq!(args.dependency_alias.as_deref(), Some("common"));
            assert_eq!(args.project.root, PathBuf::from("project"));
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "update"]);
    match cli.command {
        TopLevelCommand::Update(args) => assert!(args.dependency_alias.is_none()),
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn parses_login_logout_and_whoami_top_level_commands() {
    let cli = Cli::parse_from(["trellis", "login", "https://trellis.example.com"]);
    match cli.command {
        TopLevelCommand::Login(args) => {
            assert_eq!(args.trellis_url, "https://trellis.example.com");
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "login", "http://tsd.oats:8090"]);
    match cli.command {
        TopLevelCommand::Login(args) => {
            assert_eq!(args.trellis_url, "http://tsd.oats:8090");
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "logout"]);
    assert!(matches!(cli.command, TopLevelCommand::Logout));

    let cli = Cli::parse_from(["trellis", "whoami"]);
    assert!(matches!(cli.command, TopLevelCommand::Whoami));
}

#[test]
fn parses_remote_add_and_publish_commands() {
    let cli = Cli::parse_from([
        "trellis",
        "add",
        "acme.orders@v1",
        "--version",
        "^1.4",
        "--registry",
        "oats-center",
    ]);
    match cli.command {
        TopLevelCommand::Add(args) => {
            assert_eq!(args.source, "acme.orders@v1");
            assert_eq!(args.version.as_deref(), Some("^1.4"));
            assert_eq!(args.registry.as_deref(), Some("oats-center"));
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "publish", "--registry", "oats-center"]);
    match cli.command {
        TopLevelCommand::Publish(args) => assert_eq!(args.registry.as_deref(), Some("oats-center")),
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn rejects_users_edit_conflicting_active_flags() {
    let error = Cli::try_parse_from([
        "trellis",
        "users",
        "edit",
        "user_123",
        "--active",
        "--inactive",
    ])
    .expect_err("active flags conflict");

    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn parses_service_and_device_list_commands() {
    let cli = Cli::parse_from(["trellis", "svc", "list", "--disabled"]);
    match cli.command {
        TopLevelCommand::Svc(command) => match command.command {
            SvcSubcommand::List(args) => assert!(args.disabled),
            other => panic!("unexpected svc command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "dev", "list"]);
    match cli.command {
        TopLevelCommand::Dev(command) => match command.command {
            DevSubcommand::List(args) => assert!(!args.disabled),
            other => panic!("unexpected dev command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn parses_resources_and_events_pagination_commands() {
    assert!(matches!(
        Cli::try_parse_from(["trellis", "resources", "list", "--all", "--limit", "25"])
            .expect("resources list parses")
            .command,
        TopLevelCommand::Resources(ResourcesCommand::List(ResourceListArgs {
            all: true,
            limit: Some(25),
            ..
        }))
    ));
    assert!(matches!(
        Cli::try_parse_from([
            "trellis",
            "events",
            "dead-letters",
            "query",
            "--cursor",
            "next"
        ])
        .expect("dead-letter query parses")
        .command,
        TopLevelCommand::Events(EventsCommand::DeadLetters(DeadLettersCommand::Query(
            DeadLetterQueryArgs {
                page: PageArgs {
                    cursor: Some(_),
                    ..
                },
                ..
            }
        )))
    ));
}

#[test]
fn parses_keys_version_and_completion() {
    let cli = Cli::parse_from(["trellis", "keys", "new", "--seed", "abc"]);
    match cli.command {
        TopLevelCommand::Keys(command) => match command.command {
            KeysSubcommand::New(args) => assert_eq!(args.seed.as_deref(), Some("abc")),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "version"]);
    assert!(matches!(cli.command, TopLevelCommand::Version));

    let cli = Cli::parse_from(["trellis", "completion", "bash"]);
    assert!(matches!(cli.command, TopLevelCommand::Completion { .. }));
}
