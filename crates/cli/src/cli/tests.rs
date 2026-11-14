use super::*;
use clap::{CommandFactory, Parser};

/// Clap checks a command's options only when that command runs, and a
/// conflict (such as a global option sharing a subcommand option's name)
/// panics there; check every command at once.
#[test]
fn every_command_has_consistent_options() {
    Cli::command().debug_assert();
}

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

/// The server binds a cursor to the query's filters. Following the next-page
/// hint must run the same filtered query from the new cursor, with an earlier
/// cursor replaced and an argument that needs quoting kept intact.
#[test]
fn next_page_hint_repeats_the_filtered_query_from_the_new_cursor() {
    let ran = [
        "events",
        "dead-letters",
        "query",
        "--resource-id",
        "consumer 'one'",
        "--cursor=first",
        "--limit",
        "1",
    ]
    .map(String::from);
    let hint = crate::output::next_page_command(&ran, "second");
    let words = shlex::split(&hint).expect("the hint is one shell command");
    let cli = Cli::try_parse_from(words).expect("the hint parses");
    match cli.command {
        TopLevelCommand::Events(EventsCommand::DeadLetters(DeadLettersCommand::Query(args))) => {
            assert_eq!(args.resource_id.as_deref(), Some("consumer 'one'"));
            assert_eq!(args.page.cursor.as_deref(), Some("second"));
            assert_eq!(args.page.limit, Some(1));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}
