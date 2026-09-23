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
            assert!(!args.allow_insecure_origin);
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from([
        "trellis",
        "login",
        "http://tsd.oats:8090",
        "--allow-insecure-origin",
    ]);
    match cli.command {
        TopLevelCommand::Login(args) => {
            assert_eq!(args.trellis_url, "http://tsd.oats:8090");
            assert!(args.allow_insecure_origin);
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
fn parses_identity_grants_revoke_participant_id_positional() {
    let cli = Cli::parse_from([
        "trellis",
        "identity",
        "grants",
        "revoke",
        "igrnt_123",
        "--user",
        "user_123",
    ]);

    match cli.command {
        TopLevelCommand::Identity(command) => match command.command {
            IdentitySubcommand::Grants(command) => match command.command {
                IdentityGrantsSubcommand::Revoke(args) => {
                    assert_eq!(args.participant_id, "igrnt_123");
                    assert_eq!(args.user.as_deref(), Some("user_123"));
                }
                other => panic!("unexpected identity grants command: {other:?}"),
            },
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn parses_participant_install_and_issuer_revoke() {
    let cli = Cli::parse_from([
        "trellis",
        "participants",
        "install",
        "--source",
        ".",
        "--participant",
        "acme.app@v1",
        "--expected-revision",
        "2",
        "--platform-trust",
    ]);
    match cli.command {
        TopLevelCommand::Participants(command) => match command.command {
            ParticipantsSubcommand::Install(args) => {
                assert_eq!(args.source, PathBuf::from("."));
                assert_eq!(args.participant.as_deref(), Some("acme.app@v1"));
                assert_eq!(args.expected_revision, Some(2));
                assert!(args.platform_trust);
            }
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "participants", "install", "--source", "."]);
    match cli.command {
        TopLevelCommand::Participants(command) => match command.command {
            ParticipantsSubcommand::Install(args) => assert!(!args.platform_trust),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from([
        "trellis",
        "issuers",
        "revoke",
        "issuer_123",
        "--reason",
        "rotated",
    ]);
    match cli.command {
        TopLevelCommand::Issuers(command) => match command.command {
            IssuersSubcommand::Revoke(args) => {
                assert_eq!(args.key_id, "issuer_123");
                assert_eq!(args.reason.as_deref(), Some("rotated"));
            }
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn parses_users_create_and_edit_options() {
    let cli = Cli::parse_from([
        "trellis",
        "users",
        "create",
        "--name",
        "Ada Lovelace",
        "--email",
        "ada@example.com",
        "--username",
        "ada",
        "--inactive",
    ]);

    match cli.command {
        TopLevelCommand::Users(command) => match command.command {
            UsersSubcommand::Create(args) => {
                assert_eq!(args.name.as_deref(), Some("Ada Lovelace"));
                assert_eq!(args.email.as_deref(), Some("ada@example.com"));
                assert_eq!(args.username.as_deref(), Some("ada"));
                assert!(args.inactive);
            }
            other => panic!("unexpected users command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "users", "edit", "user_123", "--active"]);
    match cli.command {
        TopLevelCommand::Users(command) => match command.command {
            UsersSubcommand::Edit(args) => {
                assert_eq!(args.user_id, "user_123");
                assert!(args.active);
            }
            other => panic!("unexpected users command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn parses_portal_admin_commands() {
    let cli = Cli::parse_from(["trellis", "portals", "list"]);
    match cli.command {
        TopLevelCommand::Portals(command) => {
            assert!(matches!(command.command, PortalsSubcommand::List));
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "portals", "login", "default"]);
    match cli.command {
        TopLevelCommand::Portals(command) => match command.command {
            PortalsSubcommand::Login(login) => {
                assert!(matches!(login.command, PortalsLoginSubcommand::Default));
            }
            other => panic!("unexpected portals command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "portals", "login", "selection"]);
    match cli.command {
        TopLevelCommand::Portals(command) => match command.command {
            PortalsSubcommand::Login(login) => {
                assert!(matches!(login.command, PortalsLoginSubcommand::Selection));
            }
            other => panic!("unexpected portals command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }
}

#[test]
fn rejects_removed_top_level_grant_commands() {
    assert!(Cli::try_parse_from(["trellis", "grants", "list"]).is_err());
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
fn rejects_removed_service_create_namespace() {
    assert!(
        Cli::try_parse_from(["trellis", "svc", "example", "create", "--namespace", "acme"])
            .is_err()
    );
}

#[test]
fn service_and_device_help_shows_native_target_first_usage() {
    let svc_error = Cli::try_parse_from(["trellis", "svc", "--help"])
        .expect_err("svc help should render as a clap error");
    let svc_help = svc_error.to_string();
    assert!(svc_help.contains("Usage: trellis svc list [OPTIONS]"));
    assert!(svc_help.contains("trellis svc <ID> <COMMAND>"));
    assert!(svc_help.contains("<ID> and <COMMAND> are required"));
    assert!(!svc_help.contains("[ID]"));
    assert!(svc_help.contains("apply"));
    assert!(!svc_help.contains("grants"));

    let dev_error = Cli::try_parse_from(["trellis", "dev", "--help"])
        .expect_err("dev help should render as a clap error");
    let dev_help = dev_error.to_string();
    assert!(dev_help.contains("Usage: trellis dev list [OPTIONS]"));
    assert!(dev_help.contains("trellis dev <ID> <COMMAND>"));
    assert!(dev_help.contains("<ID> and <COMMAND> are required"));
    assert!(!dev_help.contains("[ID]"));
    assert!(dev_help.contains("activations"));
    assert!(dev_help.contains("reviews"));
    assert!(!dev_help.contains("grants"));

    let apply_error = Cli::try_parse_from(["trellis", "svc", "api", "apply", "--help"])
        .expect_err("svc action help should render as a clap error");
    assert!(apply_error
        .to_string()
        .contains("Usage: trellis svc <ID> apply"));
}

#[test]
fn parses_target_first_service_and_device_resource_tokens() {
    let cli = Cli::parse_from([
        "trellis",
        "svc",
        "api",
        "apply",
        "--source",
        ".",
        "--participant",
        "acme.api@v1",
        "--confirm-digest",
        "digest_123",
    ]);
    match cli.command {
        TopLevelCommand::Svc(command) => {
            assert_eq!(command.id.as_deref(), Some("api"));
            match command.command {
                SvcSubcommand::Resource(SvcResourceAction::Apply(args)) => {
                    assert_eq!(args.source, PathBuf::from("."));
                    assert_eq!(args.participant.as_deref(), Some("acme.api@v1"));
                    assert_eq!(args.confirm_digest.as_deref(), Some("digest_123"));
                }
                other => panic!("unexpected svc command: {other:?}"),
            }
        }
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from([
        "trellis",
        "dev",
        "reader",
        "reviews",
        "approve",
        "review_123",
        "--reason",
        "approved_by_policy",
    ]);
    match cli.command {
        TopLevelCommand::Dev(command) => {
            assert_eq!(command.id.as_deref(), Some("reader"));
            match command.command {
                DevSubcommand::Resource(DevResourceAction::Reviews(
                    DevReviewsCommand::Approve(args),
                )) => {
                    assert_eq!(args.review_id, "review_123");
                    assert_eq!(args.reason.as_deref(), Some("approved_by_policy"));
                }
                other => panic!("unexpected dev command: {other:?}"),
            }
        }
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
fn rejects_resource_local_grant_commands() {
    let error = Cli::try_parse_from(["trellis", "svc", "billing", "grants", "list"])
        .expect_err("svc grants should not parse");
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);

    let error = Cli::try_parse_from(["trellis", "dev", "reader", "grants", "list"])
        .expect_err("dev grants should not parse");
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
#[cfg(feature = "runtime")]
fn parses_init_config_infra_init_keys_upgrade_version_and_completion() {
    let cli = Cli::parse_from([
        "trellis",
        "init",
        "config",
        "--out",
        "./trellis",
        "--name",
        "Acme Trellis",
        "--operator-name",
        "LOCAL",
        "--system-account",
        "SYSTEM",
        "--server-name",
        "nats-local",
    ]);
    match cli.command {
        TopLevelCommand::Init(command) => match command.command {
            InitSubcommand::Config(args) => {
                assert_eq!(args.out, std::path::PathBuf::from("./trellis"));
                assert_eq!(args.name, "Acme Trellis");
                assert_eq!(args.operator_name, "LOCAL");
                assert_eq!(args.system_account, "SYSTEM");
                assert_eq!(args.server_name.as_deref(), Some("nats-local"));
                assert_eq!(args.trellis_port, 3000);
            }
            other => panic!("unexpected init command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "init", "config", "--out", "./trellis"]);
    match cli.command {
        TopLevelCommand::Init(command) => match command.command {
            InitSubcommand::Config(args) => {
                assert_eq!(args.out, std::path::PathBuf::from("./trellis"));
                assert_eq!(args.name, trellis_bootstrap::DEFAULT_TRELLIS_NAME);
                assert_eq!(args.operator_name, trellis_bootstrap::DEFAULT_OPERATOR_NAME);
                assert_eq!(
                    args.system_account,
                    trellis_bootstrap::DEFAULT_SYSTEM_ACCOUNT
                );
                assert_eq!(args.server_name, None);
                assert_eq!(args.nats_port, 4222);
                assert_eq!(args.nats_monitor_port, 8222);
                assert_eq!(args.nats_ws_port, 8080);
                assert_eq!(args.nats_server_url, None);
                assert_eq!(args.nats_websocket_url, None);
            }
            other => panic!("unexpected init command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from([
        "trellis",
        "init",
        "config",
        "--out",
        "./trellis",
        "--nats-port",
        "4322",
        "--nats-monitor-port",
        "8322",
        "--nats-ws-port",
        "8180",
    ]);
    match cli.command {
        TopLevelCommand::Init(command) => match command.command {
            InitSubcommand::Config(args) => {
                assert_eq!(args.nats_port, 4322);
                assert_eq!(args.nats_monitor_port, 8322);
                assert_eq!(args.nats_ws_port, 8180);
            }
            other => panic!("unexpected init command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    assert!(Cli::try_parse_from(["trellis", "infra", "apply"]).is_err());
    assert!(Cli::try_parse_from(["trellis", "infra", "check"]).is_err());

    let cli = Cli::parse_from([
        "trellis",
        "init",
        "admin",
        "--identity",
        "github:ada",
        "--db-path",
        "/tmp/trellis.sqlite",
    ]);
    match cli.command {
        TopLevelCommand::Init(command) => match command.command {
            InitSubcommand::Admin(args) => {
                assert_eq!(args.identity, "github:ada");
                assert_eq!(
                    args.db_path,
                    std::path::PathBuf::from("/tmp/trellis.sqlite")
                );
            }
            other => panic!("unexpected init command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "keys", "new", "--seed", "abc"]);
    match cli.command {
        TopLevelCommand::Keys(command) => match command.command {
            KeysSubcommand::New(args) => assert_eq!(args.seed.as_deref(), Some("abc")),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "upgrade", "install", "--prerelease"]);
    match cli.command {
        TopLevelCommand::Upgrade(command) => match command.command {
            UpgradeSubcommand::Install(args) => assert!(args.prerelease),
            other => panic!("unexpected upgrade command: {other:?}"),
        },
        other => panic!("unexpected top-level command: {other:?}"),
    }

    let cli = Cli::parse_from(["trellis", "version"]);
    assert!(matches!(cli.command, TopLevelCommand::Version));

    let cli = Cli::parse_from(["trellis", "completion", "bash"]);
    assert!(matches!(cli.command, TopLevelCommand::Completion { .. }));
}

#[test]
fn rejects_removed_top_level_command_trees_and_aliases() {
    for command in [
        "auth",
        "deploy",
        "deployment",
        "deployments",
        "dep",
        "d",
        "bootstrap",
        "local",
        "self",
        "keygen",
    ] {
        let error = Cli::try_parse_from(["trellis", command, "--help"])
            .expect_err(&format!("{command} should be rejected"));
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }
}

#[test]
fn rejects_legacy_auth_login_flags() {
    let error = Cli::try_parse_from(["trellis", "login", "--auth-url", "https://auth.example.com"])
        .unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(error.to_string().contains("--auth-url"));
}
