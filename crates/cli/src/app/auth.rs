use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::*;
use crate::output;
use miette::IntoDiagnostic;
use qrcode::{render::unicode, QrCode};
use serde_json::{json, Value};
use trellis_rs::auth as authlib;
use trellis_rs::generated::AppRequestDescriptor;
use trellis_runtime_apis::__types::Nullable;
use trellis_runtime_apis::types as auth_types;
use ulid::Ulid;

pub(crate) fn render_agent_login_instructions(
    login_url: &str,
    user_code: &str,
) -> miette::Result<String> {
    use owo_colors::OwoColorize as _;
    let qr = QrCode::new(login_url.as_bytes()).into_diagnostic()?;
    let qr = qr.render::<unicode::Dense1x2>().quiet_zone(false).build();
    Ok(format!(
        "Open this page to sign in, or scan the code with your phone:\n\n  {}\n\n{qr}\n\n\
        Check that the page shows the code {}.",
        login_url.cyan().underline(),
        user_code.bold()
    ))
}

pub(crate) fn pending_agent_login_json(login_url: &str) -> Value {
    json!({
        "status": "pending",
        "loginUrl": login_url,
    })
}

pub(super) async fn login(format: OutputFormat, args: &LoginArgs) -> miette::Result<()> {
    login_command(format, args).await
}

pub(super) async fn logout(format: OutputFormat) -> miette::Result<()> {
    logout_command(format).await
}

pub(super) async fn whoami(format: OutputFormat) -> miette::Result<()> {
    status_command(format).await
}

pub(super) async fn issuers(format: OutputFormat, command: IssuersCommand) -> miette::Result<()> {
    match command.command {
        IssuersSubcommand::Revoke(args) => issuers_revoke_command(format, &args).await,
    }
}

pub(super) async fn users(format: OutputFormat, command: UsersCommand) -> miette::Result<()> {
    match command.command {
        UsersSubcommand::List => users_list_command(format).await,
        UsersSubcommand::Show(args) => users_show_command(format, &args).await,
        UsersSubcommand::Find(args) => users_find_command(format, &args).await,
        UsersSubcommand::Create(args) => users_create_command(format, &args).await,
        UsersSubcommand::Edit(args) => users_edit_command(format, &args).await,
        UsersSubcommand::Delete(args) => users_delete_command(format, &args).await,
        UsersSubcommand::Link(args) => users_identity_command(format, &args, false).await,
        UsersSubcommand::Unlink(args) => users_identity_command(format, &args, true).await,
    }
}

pub(super) async fn roles(format: OutputFormat, command: RolesCommand) -> miette::Result<()> {
    match command.command {
        RolesSubcommand::List => roles_list_command(format).await,
        RolesSubcommand::Show(args) => roles_show_command(format, &args).await,
        RolesSubcommand::Create(args) => roles_create_command(format, &args).await,
        RolesSubcommand::Edit(args) => roles_edit_command(format, &args).await,
        RolesSubcommand::Delete(args) => roles_delete_command(format, &args).await,
        RolesSubcommand::Assign(args) => roles_assign_command(format, &args).await,
        RolesSubcommand::Revoke(args) => roles_revoke_command(format, &args).await,
    }
}

pub(super) async fn clients(format: OutputFormat, command: ClientsCommand) -> miette::Result<()> {
    // Settings open their own connection; the saved login holds one at a time.
    let command = match command {
        ClientsCommand::Config { client_id, args } => {
            return super::config::run(format, super::config::Scope::Client(&client_id), args).await
        }
        command => command,
    };
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    match command {
        ClientsCommand::List { cursor } => {
            let page = auth
                .o_auth_clients_list(&auth_types::AuthPageRequest {
                    search: None,
                    page: Some(trellis_runtime_apis::CursorQuery {
                        cursor,
                        limit: Some(100),
                    }),
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &page, |page| {
                output::table(
                    output::list(page, "items"),
                    &[
                        output::col("Client", "clientId"),
                        output::col("Name", "title"),
                        output::col("Kind", "kind"),
                        output::state_col("Enabled", "enabled"),
                    ],
                    "No apps are registered.",
                )?;
                output::more_pages(page);
                Ok(())
            })
        }
        ClientsCommand::Get { client_id } => {
            let response = auth
                .o_auth_clients_get(&auth_types::AuthOAuthClientGetRequest {
                    client_id: wire(client_id)?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |response| {
                print_client(&response["client"]);
                Ok(())
            })
        }
        ClientsCommand::Config { .. } => unreachable!("handled before connecting"),
        ClientsCommand::Put { input, source, app } => {
            let request: auth_types::AuthOAuthClientPutRequest =
                serde_json::from_slice(&read_input(&input)?).into_diagnostic()?;
            // Everything local is checked before the first write, so a bad
            // source leaves the stored registration as it was.
            let declaration =
                settings_declaration(source.as_deref(), app.as_deref(), &request.client_id)?;
            let response = auth.o_auth_clients_put(&request).await.into_diagnostic()?;
            if let Some(declaration) = declaration {
                if let Err(error) = auth.config_apps_declare(&declaration).await {
                    return Err(miette::miette!(
                        help = format!(
                            "Run the same command again with {}: the saved registration \
                            replays unchanged and the settings are attached.",
                            input.display()
                        ),
                        "Saved app {}, but could not attach its settings: {error}",
                        request.client_id
                    ));
                }
            }
            output::emit(format, &response, |response| {
                let client = &response["client"];
                output::success(format!(
                    "Saved app {} (revision {})",
                    output::strong(output::field(client, "clientId")),
                    output::field(client, "revision")
                ));
                Ok(())
            })
        }
        ClientsCommand::Add {
            client_id,
            settings,
        } => save_client(format, &auth, client_id, settings, None).await,
        ClientsCommand::Edit {
            client_id,
            settings,
        } => {
            let current = auth
                .o_auth_clients_get(&auth_types::AuthOAuthClientGetRequest {
                    client_id: wire(&client_id)?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?
                .client;
            save_client(format, &auth, Some(client_id), settings, Some(current)).await
        }
        ClientsCommand::SignUp(args) => {
            let client = auth
                .o_auth_clients_get(&auth_types::AuthOAuthClientGetRequest {
                    client_id: wire(&args.client_id)?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?
                .client;
            let choose = |on: bool, off: bool, current: bool| on || (current && !off);
            let response = auth
                .o_auth_clients_sign_up_update(&auth_types::AuthOAuthClientSignUpUpdateRequest {
                    client_id: client.client_id,
                    sign_up: auth_types::AuthOAuthClientSignUp {
                        password: choose(args.password, args.no_password, client.sign_up.password),
                        providers: choose(
                            args.providers,
                            args.no_providers,
                            client.sign_up.providers,
                        ),
                        extra: Default::default(),
                    },
                    expected_revision: client.revision,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |response| {
                let client = &response["client"];
                output::success(format!(
                    "Updated sign-up for {}",
                    output::strong(output::field(client, "clientId"))
                ));
                print_client(client);
                Ok(())
            })
        }
        ClientsCommand::Disable(args) => {
            let client_id = args.client_id.clone();
            let response = auth
                .o_auth_clients_disable(&auth_types::AuthOAuthClientMutationRequest {
                    client_id: wire(args.client_id)?,
                    expected_revision: wire(args.expected_revision.to_string())?,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |_| {
                output::success(format!("Disabled app {}", output::strong(&client_id)));
                Ok(())
            })
        }
        ClientsCommand::Delete(args) => {
            let client_id = args.client_id.clone();
            let response = auth
                .o_auth_clients_delete(&auth_types::AuthOAuthClientMutationRequest {
                    client_id: wire(args.client_id)?,
                    expected_revision: wire(args.expected_revision.to_string())?,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |_| {
                output::success(format!("Deleted app {}", output::strong(&client_id)));
                Ok(())
            })
        }
    }
}

fn print_client(client: &Value) {
    let sign_up = [
        ("password", "password"),
        ("identity providers", "providers"),
    ]
    .into_iter()
    .filter(|(_, key)| client["signUp"][key] == json!(true))
    .map(|(name, _)| name)
    .collect::<Vec<_>>();
    output::Details::new(output::field(client, "title"))
        .field("Client", output::field(client, "clientId"))
        .field("Kind", output::field(client, "kind"))
        .field("Description", output::field(client, "description"))
        .state("Enabled", &output::field(client, "enabled"))
        .field("Development", output::field(client, "development"))
        .field(
            "Sign-up",
            if sign_up.is_empty() {
                "off".to_owned()
            } else {
                sign_up.join(", ")
            },
        )
        .field("Revision", output::field(client, "revision"))
        .section("Redirect URIs")
        .items(
            output::list(client, "redirectUris")
                .iter()
                .map(|uri| output::field(uri, "")),
        )
        .section("Allowed without asking")
        .items(
            output::list(client, "impliedConsent")
                .iter()
                .map(|id| output::field(id, "")),
        )
        .print();
}

/// A request file's bytes, or standard input's for `-`.
fn read_input(path: &std::path::Path) -> miette::Result<Vec<u8>> {
    if path == std::path::Path::new("-") {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes).into_diagnostic()?;
        Ok(bytes)
    } else {
        std::fs::read(path).into_diagnostic()
    }
}

/// The settings declaration for `client_id` compiled from an app's source
/// project, checked before any write.
fn settings_declaration(
    source: Option<&std::path::Path>,
    app: Option<&str>,
    client_id: &str,
) -> miette::Result<Option<auth_types::AuthConfigAppDeclareRequest>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let root = source.canonicalize().into_diagnostic()?;
    let manifest = trellis_idl::project::read_manifest(&root.join("trellis.toml"))?;
    let compiled = crate::package::compile_project(&root, &manifest)?;
    let declaring = compiled
        .root_package()
        .app_requests()
        .values()
        .filter(|request| match app {
            Some(id) => request.identity().as_str() == id,
            None => !request.config().is_empty(),
        })
        .map(|request| request.identity().as_str().to_owned())
        .collect::<Vec<_>>();
    let [request_id] = declaring.as_slice() else {
        return Err(miette::miette!(
            help = "Name the app with --app.",
            "{} in {}",
            if declaring.is_empty() {
                "no app declares settings".to_owned()
            } else {
                format!("several apps declare settings: {}", declaring.join(", "))
            },
            source.display()
        ));
    };
    Ok(Some(wire(serde_json::json!({
        "clientId": client_id,
        "packageEvidence": super::deploy::package_evidence(&compiled)?,
        "requestId": request_id,
        "idempotencyKey": ulid::Ulid::generate().to_string(),
    }))?))
}

/// Whether Auth accepts `uri` as a redirect URI for this kind of app. Auth
/// checks again, including the origins Trellis itself serves.
fn check_redirect(uri: &str, kind: ClientKind, development: bool) -> Result<(), String> {
    let parsed = url::Url::parse(uri).map_err(|_| {
        format!("{uri} is not a full URL, such as https://app.example.com/callback")
    })?;
    if parsed.host_str().is_none() {
        return Err(format!("{uri} has no host"));
    }
    if parsed.fragment().is_some() {
        return Err(format!("{uri} must not have a #fragment"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!("{uri} must not hold a user name or password"));
    }
    let loopback = match parsed.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if loopback && (development || kind == ClientKind::Native) => Ok(()),
        "http" if loopback => Err(format!(
            "{uri}: plain http on localhost is only for development apps; \
            mark the app as one, or use https"
        )),
        _ => Err(format!(
            "{uri} must use https; plain http works only on localhost, \
            for a development or native app"
        )),
    }
}

/// Ask for, confirm, and save an app registration: a new one, or `current`
/// with its values as the starting answers.
async fn save_client(
    format: OutputFormat,
    auth: &trellis_runtime_apis::apps::trellis_cli::trellis_auth_v1::Client,
    client_id: Option<String>,
    settings: ClientSettingsArgs,
    current: Option<auth_types::AuthOAuthClient>,
) -> miette::Result<()> {
    // An edit asks every question when no setting is given as a flag; an
    // edit given some keeps the rest, as `roles edit` does.
    let asking = match &current {
        Some(_) => {
            settings.kind.is_none()
                && settings.title.is_none()
                && settings.description.is_none()
                && crate::prompt::pair(settings.development, settings.no_development).is_none()
                && settings.redirect_uris.is_empty()
                && settings.implied_consent.is_empty()
                && crate::prompt::pair(settings.sign_up_password, settings.no_sign_up_password)
                    .is_none()
                && crate::prompt::pair(settings.sign_up_providers, settings.no_sign_up_providers)
                    .is_none()
                && settings.source.is_none()
        }
        None => {
            client_id.is_none()
                || settings.kind.is_none()
                || settings.title.is_none()
                || (settings.redirect_uris.is_empty() && settings.kind != Some(ClientKind::Native))
        }
    };
    let mut form = crate::prompt::Form::new(asking);
    let client_id = form.required(
        "App ID (its OAuth client ID, such as acme.orders-web)",
        "<CLIENT_ID>",
        client_id,
        None,
        |id| {
            if id.chars().count() > 2048 {
                Err("Use at most 2048 characters.".to_owned())
            } else {
                Ok(())
            }
        },
    )?;
    let kinds = [
        (
            ClientKind::Browser,
            "browser: a web app; users come back to it at a redirect URI".to_owned(),
        ),
        (
            ClientKind::Native,
            "native: a desktop, mobile, or command-line app; it can also sign in \
            with a code shown on another device"
                .to_owned(),
        ),
    ];
    // A registration's kind cannot change, so an edit does not ask for it.
    let kind = match (&current, settings.kind) {
        (Some(client), None) => match client.kind {
            auth_types::AuthPublicClientKind::Native => ClientKind::Native,
            _ => ClientKind::Browser,
        },
        (_, given) => form.choose("Kind of app", "--kind", given, &kinds, None)?,
    };
    let title = form.required(
        "Title users see when the app asks them to sign in",
        "--title",
        settings.title,
        current.as_ref().map(|client| client.title.clone()),
        |title| {
            if title.chars().count() > 256 {
                Err("Use at most 256 characters.".to_owned())
            } else {
                Ok(())
            }
        },
    )?;
    let description = form.optional(
        "Description (optional)",
        settings.description,
        current
            .as_ref()
            .map(|client| client.description.clone())
            .unwrap_or_default(),
        |_| Ok(()),
    )?;
    form.explain(
        "A development app may send users back to plain http://localhost, \
        127.0.0.1, or [::1]. Apps users rely on use https.",
    );
    let development = form.yes_no(
        "Is this a development app?",
        crate::prompt::pair(settings.development, settings.no_development),
        current.as_ref().is_some_and(|client| client.development),
    )?;
    form.explain(&format!(
        "Redirect URIs are where users return after signing in, matched exactly. \
        Use https, or http://localhost:<port>/... for a {} app. Separate several with spaces.{}",
        if kind == ClientKind::Native {
            "native"
        } else {
            "development"
        },
        if kind == ClientKind::Native {
            " Leave it empty for an app that signs in only with a code."
        } else {
            ""
        }
    ));
    let redirect_uris = form.list(
        "Redirect URIs",
        "--redirect-uri",
        settings.redirect_uris,
        current
            .as_ref()
            .map(|client| client.redirect_uris.clone())
            .unwrap_or_default(),
        kind == ClientKind::Browser,
        |uri| check_redirect(uri, kind, development),
    )?;
    form.explain(
        "Capabilities the app gets without asking users, such as \
        acme.orders.v1:orders.read. Leave it empty to ask users for everything.",
    );
    let implied_consent = form.list(
        "Allowed without asking (optional)",
        "--allow-without-asking",
        settings.implied_consent,
        current
            .as_ref()
            .map(|client| {
                client
                    .implied_consent
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        false,
        |_| Ok(()),
    )?;
    form.explain("Sign-up lets users without an account create one while signing in to this app.");
    let current_sign_up = current.as_ref().map_or((false, false), |client| {
        (client.sign_up.password, client.sign_up.providers)
    });
    let sign_up_password = form.yes_no(
        "Let users create a username-and-password account? \
        (The installation must allow it too: trellis sign-in set --local-sign-up)",
        crate::prompt::pair(settings.sign_up_password, settings.no_sign_up_password),
        current_sign_up.0,
    )?;
    let sign_up_providers = form.yes_no(
        "Let a first sign-in with an OIDC identity provider create the account?",
        crate::prompt::pair(settings.sign_up_providers, settings.no_sign_up_providers),
        current_sign_up.1,
    )?;
    let asked_settings = settings.source.is_none() && form.is_interactive();
    form.explain(
        "If the app declares settings in a Trellis source project, give the \
        project's directory so administrators can set them. Leave it empty if not.",
    );
    let source = form.optional(
        "Source project with the app's settings (optional)",
        settings.source.map(|path| path.display().to_string()),
        String::new(),
        |path| {
            if std::path::Path::new(path).join("trellis.toml").is_file() {
                Ok(())
            } else {
                Err(format!("{path} holds no trellis.toml"))
            }
        },
    )?;
    let source = (!source.is_empty()).then(|| std::path::PathBuf::from(source));
    let app = match (&source, settings.app) {
        (Some(_), None) if asked_settings => {
            let app = form.optional(
                "The app in that project that declares the settings \
                (empty when only one does)",
                None,
                String::new(),
                |_| Ok(()),
            )?;
            (!app.is_empty()).then_some(app)
        }
        (_, app) => app,
    };
    form.finish()?;
    for uri in &redirect_uris {
        check_redirect(uri, kind, development).map_err(|message| miette::miette!("{message}"))?;
    }
    let declaration = settings_declaration(source.as_deref(), app.as_deref(), &client_id)?;
    let yes_no = |value: bool| if value { "yes" } else { "no" }.to_owned();
    form.confirm(
        &format!(
            "{} app {client_id}",
            if current.is_some() {
                "Save"
            } else {
                "Register"
            }
        ),
        &[
            (
                "Kind",
                match kind {
                    ClientKind::Browser => "browser",
                    ClientKind::Native => "native",
                }
                .to_owned(),
            ),
            ("Title", title.clone()),
            ("Description", description.clone()),
            ("Development", yes_no(development)),
            ("Redirect URIs", redirect_uris.join(" ")),
            ("Allowed without asking", implied_consent.join(" ")),
            ("Password sign-up", yes_no(sign_up_password)),
            ("Provider sign-up", yes_no(sign_up_providers)),
            (
                "Settings from",
                source
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            ),
        ],
    )?;
    let mut client = auth
        .o_auth_clients_put(&auth_types::AuthOAuthClientPutRequest {
            client_id: wire(&client_id)?,
            kind: match kind {
                ClientKind::Browser => auth_types::AuthPublicClientKind::Browser,
                ClientKind::Native => auth_types::AuthPublicClientKind::Native,
            },
            title: wire(&title)?,
            description: wire(&description)?,
            redirect_uris,
            development,
            implied_consent: wire(&implied_consent)?,
            expected_revision: current
                .as_ref()
                .map_or(0, |client| client.revision.0 .0)
                .into(),
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .client;
    let edit = format!("trellis clients edit {client_id}");
    if (client.sign_up.password, client.sign_up.providers) != (sign_up_password, sign_up_providers)
    {
        client = auth
            .o_auth_clients_sign_up_update(&auth_types::AuthOAuthClientSignUpUpdateRequest {
                client_id: client.client_id.clone(),
                sign_up: auth_types::AuthOAuthClientSignUp {
                    password: sign_up_password,
                    providers: sign_up_providers,
                    extra: Default::default(),
                },
                expected_revision: client.revision,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .map_err(|error| {
                miette::miette!(
                    help = format!("Set sign-up with `trellis clients sign-up {client_id}`."),
                    "Saved app {client_id}, but could not change its sign-up: {error}"
                )
            })?
            .client;
    }
    if let Some(declaration) = declaration {
        if let Err(error) = auth.config_apps_declare(&declaration).await {
            return Err(miette::miette!(
                help = format!("Attach them with `{edit} --source <PATH>`."),
                "Saved app {client_id}, but could not attach its settings: {error}"
            ));
        }
    }
    output::emit(format, &json!({ "client": client }), |response| {
        let client = &response["client"];
        output::success(format!(
            "Saved app {} (revision {})",
            output::strong(output::field(client, "clientId")),
            output::field(client, "revision")
        ));
        print_client(client);
        Ok(())
    })
}

pub(super) async fn apis(format: OutputFormat, command: ApisCommand) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    match command {
        ApisCommand::List { cursor } => {
            let page = auth
                .apis_list(&auth_types::AuthPageRequest {
                    search: None,
                    page: Some(trellis_runtime_apis::CursorQuery {
                        cursor,
                        limit: Some(100),
                    }),
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &page, |page| {
                output::table(
                    output::list(page, "items"),
                    &[
                        output::col("API", "apiId"),
                        output::col("Name", "title"),
                        output::col("Version", "version"),
                        output::time_col("Accepted", "acceptedAt"),
                    ],
                    "No APIs are accepted yet.",
                )?;
                output::more_pages(page);
                Ok(())
            })
        }
        ApisCommand::Get { api_id } => {
            let response = auth
                .apis_get(&auth_types::AuthApiGetRequest {
                    api_id: wire(api_id)?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |response| {
                let api = &response["api"];
                output::Details::new(output::field(api, "title"))
                    .field("API", output::field(api, "apiId"))
                    .field("Version", output::field(api, "version"))
                    .field("Description", output::field(api, "description"))
                    .time("Accepted", &output::field(api, "acceptedAt"))
                    .field(
                        "Revision",
                        format!(
                            "{} (generation {})",
                            output::field(api, "acceptedRevision"),
                            output::field(api, "generation")
                        ),
                    )
                    .field("Digest", output::field(api, "canonicalDigest"))
                    .section("Actions")
                    .items(output::list(response, "actions").iter().map(|action| {
                        format!(
                            "{:<9} {}",
                            output::field(action, "kind"),
                            output::field(action, "name")
                        )
                    }))
                    .section("Capabilities")
                    .items({
                        let capabilities = output::list(api, "capabilities");
                        let width = capabilities
                            .iter()
                            .map(|capability| {
                                output::field(capability, "identity.capabilityId").len()
                            })
                            .max()
                            .unwrap_or(0);
                        capabilities.iter().map(move |capability| {
                            format!(
                                "{:<width$}  {}",
                                output::field(capability, "identity.capabilityId"),
                                output::field(capability, "title")
                            )
                        })
                    })
                    .print();
                output::hint(format!(
                    "Its canonical IDL is in {}.",
                    output::code("--format json")
                ));
                Ok(())
            })
        }
        ApisCommand::Review(args) => {
            let participant = crate::app::deploy::compile_participant_input(
                &args.source,
                args.participant.as_deref(),
                None,
            )?;
            let mut reviews = Vec::new();
            for api_id in &participant.implemented_apis {
                reviews.push(
                    auth.apis_review(&auth_types::AuthApiReviewRequest {
                        api_id: wire(api_id)?,
                        package_evidence: participant.package_evidence.clone(),
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?
                    .review,
                );
            }
            output::emit(format, &json!({ "reviews": reviews }), |value| {
                let reviews = output::list(value, "reviews");
                if reviews.is_empty() {
                    output::note(format!(
                        "{} implements no APIs.",
                        participant.participant_id
                    ));
                }
                for (index, review) in reviews.iter().enumerate() {
                    if index > 0 {
                        anstream::println!();
                    }
                    print_review(review);
                }
                if !reviews.is_empty() {
                    output::next(&format!(
                        "trellis apis accept --source {}{}",
                        args.source.display(),
                        args.participant
                            .as_deref()
                            .map(|participant| format!(" --participant {participant}"))
                            .unwrap_or_default()
                    ));
                }
                Ok(())
            })
        }
        ApisCommand::Accept(args) => {
            let participant = crate::app::deploy::compile_participant_input(
                &args.source,
                args.participant.as_deref(),
                None,
            )?;
            let mut apis = Vec::new();
            let mut outcomes = Vec::new();
            for api_id in &participant.implemented_apis {
                let review = auth
                    .apis_review(&auth_types::AuthApiReviewRequest {
                        api_id: wire(api_id)?,
                        package_evidence: participant.package_evidence.clone(),
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?
                    .review;
                if review.current_generation.is_some() {
                    let current = auth
                        .apis_get(&auth_types::AuthApiGetRequest {
                            api_id: wire(api_id)?,
                            extra: Default::default(),
                        })
                        .await
                        .into_diagnostic()?;
                    if current.api.canonical_digest == review.canonical_digest {
                        apis.push(current.api);
                        outcomes.push(false);
                        continue;
                    }
                }
                miette::ensure!(
                    review.compatible,
                    help = "Review the change with `trellis apis review`, then replace it \
                        with `trellis apis force-replace`.",
                    "API {api_id} changes incompatibly ({}); review {}",
                    review.breakage.join("; "),
                    review.review_id
                );
                let accepted = auth
                    .apis_accept(&auth_types::AuthApiAcceptRequest {
                        review_id: review.review_id,
                        canonical_digest: review.canonical_digest,
                        expected_generation: review
                            .current_generation
                            .map_or(0, |value| value.0 .0)
                            .into(),
                        expected_accepted_revision: review
                            .current_accepted_revision
                            .map_or(0, |value| value.0 .0)
                            .into(),
                        role_edits: Vec::new(),
                        mapping_edits: Vec::new(),
                        idempotency_key: wire(cli_idempotency_key())?,
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?;
                apis.push(accepted.api);
                outcomes.push(true);
            }
            output::emit(format, &json!({ "apis": apis }), |value| {
                let apis = output::list(value, "apis");
                if apis.is_empty() {
                    output::note(format!(
                        "{} implements no APIs.",
                        participant.participant_id
                    ));
                }
                for (api, changed) in apis.iter().zip(outcomes) {
                    let id = output::strong(output::field(api, "apiId"));
                    if changed {
                        output::success(format!(
                            "Accepted {id} (revision {})",
                            output::field(api, "acceptedRevision")
                        ));
                    } else {
                        output::unchanged(format!("{id} is already accepted"));
                    }
                }
                Ok(())
            })
        }
        ApisCommand::ForceReplace(args) => {
            let Some(input) = args.input else {
                return force_replace_from_source(format, &auth, args).await;
            };
            let request: auth_types::AuthApiForceReplaceRequest =
                serde_json::from_slice(&read_input(&input)?).into_diagnostic()?;
            let response = auth.apis_force_replace(&request).await.into_diagnostic()?;
            output::emit(format, &response, |response| {
                output::success(format!(
                    "Replaced {} (revision {})",
                    output::strong(output::field(response, "api.apiId")),
                    output::field(response, "api.acceptedRevision")
                ));
                print_enforcement(&response["enforcement"]);
                Ok(())
            })
        }
    }
}

/// Replace each accepted API a source package's participant changes, after
/// showing its review and getting both acknowledgements.
async fn force_replace_from_source(
    format: OutputFormat,
    auth: &trellis_runtime_apis::apps::trellis_cli::trellis_auth_v1::Client,
    args: ApisForceReplaceArgs,
) -> miette::Result<()> {
    const BREAKAGE: &str = "Replace it anyway? Callers built against the accepted \
        definition may break.";
    const CONSENT: &str = "Accept that what users agreed to may now mean something different?";
    let mut form = crate::prompt::Form::new(
        args.source.is_none() || !args.acknowledge_breakage || !args.acknowledge_consent_impact,
    );
    let source = form.required(
        "Source project directory (holds trellis.toml)",
        "--source",
        args.source.map(|path| path.display().to_string()),
        None,
        |path| {
            if std::path::Path::new(path).join("trellis.toml").is_file() {
                Ok(())
            } else {
                Err(format!("{path} holds no trellis.toml"))
            }
        },
    )?;
    // Without a terminal, both acknowledgements must be flags; name every
    // missing one before doing anything.
    let mut acknowledged = (args.acknowledge_breakage, args.acknowledge_consent_impact);
    if !form.is_interactive() {
        form.acknowledge(BREAKAGE, "--acknowledge-breakage", acknowledged.0)?;
        form.acknowledge(CONSENT, "--acknowledge-consent-impact", acknowledged.1)?;
    }
    form.finish()?;
    let participant = crate::app::deploy::compile_participant_input(
        std::path::Path::new(&source),
        args.participant.as_deref(),
        None,
    )?;
    let mut reviews = Vec::new();
    for api_id in &participant.implemented_apis {
        let review = auth
            .apis_review(&auth_types::AuthApiReviewRequest {
                api_id: wire(api_id)?,
                package_evidence: participant.package_evidence.clone(),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?
            .review;
        let (Some(generation), Some(revision)) =
            (review.current_generation, review.current_accepted_revision)
        else {
            output::note(format!(
                "{api_id} is not accepted yet; accept it with `trellis apis accept`."
            ));
            continue;
        };
        let current = auth
            .apis_get(&auth_types::AuthApiGetRequest {
                api_id: wire(api_id)?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?;
        if current.api.canonical_digest == review.canonical_digest {
            output::unchanged(format!("{api_id} is already accepted"));
            continue;
        }
        reviews.push((review, generation, revision));
    }
    miette::ensure!(
        !reviews.is_empty(),
        "{} changes no accepted API; nothing to replace",
        participant.participant_id
    );
    if form.is_interactive() {
        if !output::is_json(format) {
            for (review, _, _) in &reviews {
                print_review(&serde_json::to_value(review).into_diagnostic()?);
            }
        }
        acknowledged.0 = form.acknowledge(BREAKAGE, "--acknowledge-breakage", acknowledged.0)?;
        miette::ensure!(acknowledged.0, "cancelled; nothing changed");
        acknowledged.1 =
            form.acknowledge(CONSENT, "--acknowledge-consent-impact", acknowledged.1)?;
        miette::ensure!(acknowledged.1, "cancelled; nothing changed");
    }
    let mut replacements = Vec::new();
    for (review, generation, revision) in reviews {
        replacements.push(
            auth.apis_force_replace(&auth_types::AuthApiForceReplaceRequest {
                review_id: review.review_id,
                canonical_digest: review.canonical_digest,
                expected_generation: generation,
                expected_accepted_revision: revision,
                acknowledge_breakage: acknowledged.0,
                acknowledge_consent_impact: acknowledged.1,
                role_edits: Vec::new(),
                mapping_edits: Vec::new(),
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        );
    }
    output::emit(format, &json!({ "replacements": replacements }), |value| {
        for response in output::list(value, "replacements") {
            output::success(format!(
                "Replaced {} (revision {})",
                output::strong(output::field(response, "api.apiId")),
                output::field(response, "api.acceptedRevision")
            ));
            print_enforcement(&response["enforcement"]);
        }
        Ok(())
    })
}

/// What accepting a reviewed API would change.
fn print_review(review: &Value) {
    let compatible = review["compatible"] == json!(true);
    let current = output::field(review, "currentAcceptedRevision");
    output::Details::new(output::field(review, "apiId"))
        .field(
            "Catalog",
            if current.is_empty() {
                "new API".to_owned()
            } else {
                format!("replaces revision {current}")
            },
        )
        .field(
            "Change",
            if compatible {
                output::state("compatible").to_string()
            } else {
                format!(
                    "{}; needs {}",
                    output::state("incompatible"),
                    output::code("trellis apis force-replace")
                )
            },
        )
        .field("Review", output::field(review, "reviewId"))
        .time("Expires", &output::field(review, "expiresAt"))
        .section("Breaks")
        .items(
            output::list(review, "breakage")
                .iter()
                .map(|item| output::field(item, "")),
        )
        .section("Actions")
        .items(output::list(review, "actionChanges").iter().map(|action| {
            format!(
                "{:<9} {} ({})",
                output::field(action, "kind"),
                output::field(action, "name"),
                output::field(action, "direction")
            )
        }))
        .section("Capabilities")
        .items(
            output::list(review, "capabilityChanges")
                .iter()
                .map(|change| {
                    format!(
                        "{}  {}",
                        output::field(change, "capabilityId"),
                        output::field(change, "description")
                    )
                }),
        )
        .section("Affects")
        .items(
            [
                ("role", output::list(review, "affectedRoleIds").len()),
                (
                    "deployment",
                    output::list(review, "affectedDeploymentIds").len(),
                ),
            ]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(noun, count)| output::plural(count, noun)),
        )
        .print();
}

/// How Auth is applying a change to sessions that already exist.
fn print_enforcement(enforcement: &Value) {
    if enforcement.is_object() {
        let retired = enforcement["retiredSessions"].as_u64().unwrap_or(0);
        if retired > 0 {
            output::hint(format!(
                "Signed out {} that the change no longer allows.",
                output::plural(retired as usize, "session")
            ));
        }
    }
}

pub(super) async fn sign_in(format: OutputFormat, command: SignInCommand) -> miette::Result<()> {
    let (selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    if let SignInCommand::Providers(command) = command {
        let callback = format!(
            "{}/portal/oidc/callback",
            selection.scope.issuer.trim_end_matches('/')
        );
        return providers_command(format, &auth, &callback, command).await;
    }
    let current = auth
        .sign_in_settings_get(&auth_types::AuthEmpty {
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    let changed = matches!(command, SignInCommand::Set(_));
    let result = match command {
        SignInCommand::Show => current,
        SignInCommand::Providers(_) => unreachable!("handled above"),
        SignInCommand::Set(args) => {
            let current = current.settings;
            let choose = |on: bool, off: bool, value: bool| on || (value && !off);
            auth.sign_in_settings_update(&auth_types::AuthSignInSettingsUpdateRequest {
                local_users: choose(args.local_users, args.no_local_users, current.local_users),
                local_sign_up: choose(
                    args.local_sign_up,
                    args.no_local_sign_up,
                    current.local_sign_up,
                ),
                password_min_length: args.password_min_length.map_or(
                    current.password_min_length,
                    auth_types::AuthPasswordMinLength,
                ),
                expected_revision: current.revision,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?
        }
    };
    output::emit(format, &result, |result| {
        if changed {
            output::success("Updated how users sign in");
        }
        let settings = &result["settings"];
        let on = |key: &str| output::state(output::on_off(settings[key] == json!(true)));
        output::Details::new("Sign-in")
            .field("Username and password", on("localUsers"))
            .field("Create own account", on("localSignUp"))
            .field(
                "Minimum password length",
                output::field(settings, "passwordMinLength"),
            )
            .print();
        output::hint(format!(
            "OIDC sign-in is on while a provider is; see {}.",
            output::code("trellis sign-in providers list")
        ));
        Ok(())
    })
}

async fn providers_command(
    format: OutputFormat,
    auth: &trellis_runtime_apis::apps::trellis_cli::trellis_auth_v1::Client,
    callback: &str,
    command: ProvidersCommand,
) -> miette::Result<()> {
    let get = |provider_id: &str| {
        let request = wire::<auth_types::AuthOIDCProviderGetRequest>(json!({
            "providerId": provider_id,
        }));
        async move {
            Ok::<_, miette::Report>(
                auth.oidc_providers_get(&request?)
                    .await
                    .into_diagnostic()?
                    .provider,
            )
        }
    };
    let mutation =
        |provider: &auth_types::AuthOIDCProvider| auth_types::AuthOIDCProviderMutationRequest {
            provider_id: provider.provider_id,
            expected_revision: provider.revision,
            idempotency_key: Ulid::generate().to_string().into(),
            extra: Default::default(),
        };
    let action = match &command {
        ProvidersCommand::Add(_) => "added",
        ProvidersCommand::Edit(_) => "updated",
        ProvidersCommand::Enable { .. } => "enabled",
        ProvidersCommand::Disable { .. } => "disabled",
        _ => "shown",
    };
    let provider = match command {
        ProvidersCommand::List => {
            let mut providers = Vec::new();
            let mut cursor = None;
            loop {
                let page = auth
                    .oidc_providers_list(&auth_types::AuthPageRequest {
                        search: None,
                        page: Some(trellis_runtime_apis::CursorQuery {
                            cursor,
                            limit: Some(100),
                        }),
                        extra: Default::default(),
                    })
                    .await
                    .into_diagnostic()?;
                providers.extend(page.items);
                match page.page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            return output::emit(
                format,
                &json!({ "items": providers, "callbackUrl": callback }),
                |value| {
                    output::table(
                        output::list(value, "items"),
                        &[
                            output::muted("ID", "providerId"),
                            output::col("Name", "title"),
                            output::col("Issuer", "issuer"),
                            output::state_col("Enabled", "enabled"),
                        ],
                        "No identity providers yet.",
                    )?;
                    if providers.is_empty() {
                        output::next("trellis sign-in providers add --name <NAME> --issuer <URL> --client-id <ID>");
                    } else {
                        output::hint(format!(
                            "Callback URL to register with each provider: {callback}"
                        ));
                    }
                    Ok(())
                },
            );
        }
        ProvidersCommand::Show { provider_id } => get(&provider_id).await?,
        ProvidersCommand::Add(args) => {
            let mut form = crate::prompt::Form::new(
                args.name.is_none() || args.issuer.is_none() || args.client_id.is_none(),
            );
            let name = form.required(
                "Name on the sign-in button",
                "--name",
                args.name,
                None,
                |_| Ok(()),
            )?;
            let issuer = form.required(
                "Issuer URL, such as https://example.us.auth0.com/",
                "--issuer",
                args.issuer,
                None,
                |issuer| match url::Url::parse(issuer) {
                    Ok(url) if url.scheme() == "https" => Ok(()),
                    _ => Err("Give the provider's https:// issuer URL.".to_owned()),
                },
            )?;
            let client_id = form.required(
                "Client ID the provider gave this Trellis",
                "--client-id",
                args.client_id,
                None,
                |_| Ok(()),
            )?;
            let client_secret = match args.client_secret_file {
                Some(path) => read_client_secret(Some(&path))?,
                None => form.secret("Client secret the provider gave (empty for none)", None)?,
            };
            let scopes = form.list(
                "Scopes to request",
                "--scope",
                args.scopes,
                vec!["openid".into(), "profile".into(), "email".into()],
                true,
                |_| Ok(()),
            )?;
            form.explain("Roles can follow the groups a provider lists for each user.");
            let groups_claim = form.optional(
                "JSON pointer to the groups claim",
                args.groups_claim,
                "/groups".into(),
                |_| Ok(()),
            )?;
            form.explain(
                "Pin the provider's certificate issuer only when nothing on the \
                server vouches for its certificate. Confirm the fingerprint with \
                the provider's operator first.",
            );
            let certificate_fingerprint = form.optional(
                "Certificate issuer's SHA-256 fingerprint (optional)",
                args.certificate_fingerprint,
                String::new(),
                |_| Ok(()),
            )?;
            form.finish()?;
            form.confirm(
                &format!("Add identity provider {name}"),
                &[
                    ("Issuer", issuer.clone()),
                    ("Client ID", client_id.clone()),
                    (
                        "Client secret",
                        if client_secret.is_some() { "set" } else { "" }.to_owned(),
                    ),
                    ("Scopes", scopes.join(" ")),
                    ("Groups claim", groups_claim.clone()),
                    ("Pinned issuer", certificate_fingerprint.clone()),
                ],
            )?;
            auth.oidc_providers_put(&auth_types::AuthOIDCProviderPutRequest {
                provider_id: None,
                issuer: issuer.trim().to_owned(),
                client_id: client_id.trim().to_owned(),
                title: wire(name.trim())?,
                scopes,
                role_claim: groups_claim.trim().to_owned(),
                claim_freshness_seconds: wire(3600)?,
                client_secret,
                certificate_fingerprint: (!certificate_fingerprint.trim().is_empty())
                    .then(|| Nullable::Value(certificate_fingerprint.trim().to_owned())),
                expected_revision: 0u64.into(),
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?
            .provider
        }
        ProvidersCommand::Edit(args) => {
            let current = get(&args.provider_id).await?;
            let keep = |value: Option<String>, current: String| {
                value.map_or(current, |value| value.trim().to_owned())
            };
            auth.oidc_providers_put(&auth_types::AuthOIDCProviderPutRequest {
                provider_id: Some(current.provider_id),
                issuer: current.issuer,
                client_id: keep(args.client_id, current.client_id),
                title: wire(keep(args.name, current.title))?,
                scopes: if args.scopes.is_empty() {
                    current.scopes
                } else {
                    args.scopes
                },
                role_claim: keep(args.groups_claim, current.role_claim),
                claim_freshness_seconds: current.claim_freshness_seconds,
                // Left out, the provider keeps its secret.
                client_secret: read_client_secret(args.client_secret_file.as_deref())?,
                // Left out, the provider keeps its pinned issuer.
                certificate_fingerprint: if args.no_certificate_fingerprint {
                    Some(Nullable::Null)
                } else {
                    args.certificate_fingerprint
                        .map(|fingerprint| Nullable::Value(fingerprint.trim().to_owned()))
                },
                expected_revision: current.revision.0,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?
            .provider
        }
        ProvidersCommand::Enable { provider_id } => {
            let current = get(&provider_id).await?;
            auth.oidc_providers_enable(&mutation(&current))
                .await
                .into_diagnostic()?;
            get(&provider_id).await?
        }
        ProvidersCommand::Disable { provider_id } => {
            let current = get(&provider_id).await?;
            auth.oidc_providers_disable(&mutation(&current))
                .await
                .into_diagnostic()?;
            get(&provider_id).await?
        }
        ProvidersCommand::Remove { provider_id } => {
            let current = get(&provider_id).await?;
            auth.oidc_providers_delete(&mutation(&current))
                .await
                .into_diagnostic()?;
            return output::emit(
                format,
                &json!({ "removed": true, "providerId": provider_id }),
                |_| {
                    output::success(format!(
                        "Removed identity provider {}",
                        output::strong(&current.title)
                    ));
                    Ok(())
                },
            );
        }
    };
    output::emit(
        format,
        &json!({ "provider": provider, "callbackUrl": callback }),
        |value| {
            let provider = &value["provider"];
            let name = output::strong(output::field(provider, "title"));
            match action {
                "added" => output::success(format!("Added identity provider {name}")),
                "updated" => output::success(format!("Updated identity provider {name}")),
                "enabled" => output::success(format!("Turned on identity provider {name}")),
                "disabled" => output::success(format!("Turned off identity provider {name}")),
                _ => {}
            }
            output::Details::new(output::field(provider, "title"))
                .field("ID", output::field(provider, "providerId"))
                .state("Enabled", &output::field(provider, "enabled"))
                .field("Issuer", output::field(provider, "issuer"))
                .field("Client ID", output::field(provider, "clientId"))
                .field(
                    "Client secret",
                    if provider["hasClientSecret"] == json!(true) {
                        "set"
                    } else {
                        "none"
                    },
                )
                .field(
                    "Scopes",
                    output::list(provider, "scopes")
                        .iter()
                        .map(|scope| output::field(scope, ""))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
                .field("Groups claim", output::field(provider, "roleClaim"))
                .field(
                    "Certificate issuer",
                    provider["certificateFingerprint"]
                        .as_str()
                        .unwrap_or("system trust store"),
                )
                .field("Callback URL", callback)
                .print();
            if action == "added" {
                output::hint("Register the callback URL with the provider before anyone signs in.");
            }
            Ok(())
        },
    )
}

/// A client secret from a file, or from standard input for `-`, without its
/// trailing newline, so it never appears in the shell history or process list.
fn read_client_secret(path: Option<&std::path::Path>) -> miette::Result<Option<String>> {
    let Some(path) = path else { return Ok(None) };
    let secret = if path == std::path::Path::new("-") {
        std::io::read_to_string(std::io::stdin()).into_diagnostic()?
    } else {
        std::fs::read_to_string(path).into_diagnostic()?
    };
    let secret = secret.trim_end_matches(['\r', '\n']).to_owned();
    if secret.is_empty() {
        return Err(miette::miette!("the client secret is empty"));
    }
    Ok(Some(secret))
}

pub(super) async fn theme(format: OutputFormat, command: ThemeCommand) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let client = match &command {
        ThemeCommand::Show(args) | ThemeCommand::Remove(args) => args.client.clone(),
        ThemeCommand::Set(args) => args.client.clone(),
    };
    let current = auth
        .portal_themes_get(&auth_types::AuthPortalThemeGetRequest {
            client_id: client.clone().map(Into::into),
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    let current_revision = current
        .theme
        .as_ref()
        .map_or(0, |theme| theme.revision.0 .0);
    let target = match &client {
        Some(client) => format!("app {}", output::strong(client)),
        None => "the installation".to_owned(),
    };
    match command {
        ThemeCommand::Show(_) => output::emit(format, &current, |value| {
            let Some(theme) = value.get("theme").filter(|theme| theme.is_object()) else {
                output::note(format!(
                    "No theme is set for {target}; {}.",
                    if client.is_some() {
                        "it uses the installation's"
                    } else {
                        "the Portal uses Trellis's own look"
                    }
                ));
                return Ok(());
            };
            print_theme(theme, &target);
            Ok(())
        }),
        ThemeCommand::Remove(args) => {
            let response = auth
                .portal_themes_delete(&auth_types::AuthPortalThemeDeleteRequest {
                    client_id: client.map(Into::into),
                    expected_revision: wire(
                        args.expected_revision
                            .unwrap_or(current_revision)
                            .to_string(),
                    )?,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &response, |_| {
                output::success(format!("Removed the theme for {target}"));
                Ok(())
            })
        }
        ThemeCommand::Set(args) => {
            // NAME=VALUE pairs onto the generated model's fields, refusing
            // names the model does not have.
            let fields = |pairs: &[String], names: &[(&str, &str)]| -> miette::Result<Value> {
                let mut object = serde_json::Map::new();
                for pair in pairs {
                    let (name, value) = pair
                        .split_once('=')
                        .ok_or_else(|| miette::miette!("'{pair}' is not NAME=VALUE"))?;
                    let field = names
                        .iter()
                        .find(|(portal, _)| *portal == name)
                        .ok_or_else(|| {
                            miette::miette!(
                                "unknown name '{name}'; use one of {}",
                                names
                                    .iter()
                                    .map(|(portal, _)| *portal)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        })?
                        .1;
                    object.insert(field.to_owned(), json!(value));
                }
                Ok(Value::Object(object))
            };
            const COLORS: [(&str, &str); 10] = [
                ("page", "page"),
                ("surface", "surface"),
                ("ink", "ink"),
                ("muted", "muted"),
                ("line", "line"),
                ("line-strong", "lineStrong"),
                ("primary", "primary"),
                ("primary-hover", "primaryHover"),
                ("on-primary", "onPrimary"),
                ("soft", "soft"),
            ];
            const COPY: [(&str, &str); 3] = [
                ("sign_in_title", "signInTitle"),
                ("sign_in_subtitle", "signInSubtitle"),
                ("footer", "footer"),
            ];
            let logo = match &args.logo {
                Some(path) => {
                    let format = match path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref()
                    {
                        Some("png") => "png",
                        Some("jpg" | "jpeg") => "jpeg",
                        Some("webp") => "webp",
                        Some("svg") => "svg",
                        _ => {
                            return Err(miette::miette!(
                                "the logo must be a .png, .jpg, .webp, or .svg file"
                            ))
                        }
                    };
                    let bytes = std::fs::read(path).into_diagnostic()?;
                    Some(json!({
                        "format": format,
                        "base64": base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            bytes,
                        ),
                    }))
                }
                None => None,
            };
            let stylesheet = args
                .stylesheet
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()
                .into_diagnostic()?;
            let request: auth_types::AuthPortalThemePutRequest = serde_json::from_value(json!({
                "clientId": client,
                "name": args.name,
                "logo": logo,
                "stylesheet": stylesheet,
                "colors": fields(&args.colors, &COLORS)?,
                "darkColors": fields(&args.dark_colors, &COLORS)?,
                "copy": fields(&args.copy, &COPY)?,
                "expectedRevision": args.expected_revision.unwrap_or(current_revision).to_string(),
                "idempotencyKey": cli_idempotency_key(),
            }))
            .into_diagnostic()?;
            let response = auth.portal_themes_put(&request).await.into_diagnostic()?;
            output::emit(format, &response, |value| {
                output::success(format!("Saved the theme for {target}"));
                if let Some(theme) = value.get("theme").filter(|theme| theme.is_object()) {
                    print_theme(theme, &target);
                }
                Ok(())
            })
        }
    }
}

fn print_theme(theme: &Value, target: &str) {
    let stylesheet = theme["stylesheet"]
        .as_str()
        .map(|text| output::plural(text.len(), "byte"))
        .unwrap_or_default();
    output::Details::new(format!("Theme for {target}"))
        .field("Name", output::field(theme, "name"))
        .field("Logo", output::field(theme, "logo.format"))
        .field("Stylesheet", stylesheet)
        .field("Revision", output::field(theme, "revision"))
        .time("Updated", &output::field(theme, "updatedAt"))
        .section("Colors")
        .object(&theme["colors"])
        .section("Dark colors")
        .object(&theme["darkColors"])
        .section("Copy")
        .object(&theme["copy"])
        .print();
}

fn trimmed_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn cli_idempotency_key() -> String {
    Ulid::generate().to_string()
}

async fn users_list_command(format: OutputFormat) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let mut users = Vec::new();
    let mut cursor = None;
    loop {
        let page = auth
            .users_list(&auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?;
        users.extend(page.items);
        match page.page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    output::emit(format, &json!({ "items": users }), |value| {
        let users = output::list(value, "items");
        // Account details come only with permission to manage users.
        let managed = users.iter().any(|user| user["account"].is_object());
        let rows = users
            .iter()
            .map(|user| {
                json!({
                    "userId": user["principalId"],
                    "name": user["displayName"],
                    "username": user["account"]["username"],
                    "status": match user["account"]["principal"]["enabled"].as_bool() {
                        Some(true) => "enabled",
                        Some(false) => "disabled",
                        None => "",
                    },
                })
            })
            .collect::<Vec<_>>();
        if managed {
            output::table(
                &rows,
                &[
                    output::muted("ID", "userId"),
                    output::col("Name", "name"),
                    output::col("Username", "username"),
                    output::state_col("Status", "status"),
                ],
                "No users yet.",
            )
        } else {
            output::table(
                &rows,
                &[output::muted("ID", "userId"), output::col("Name", "name")],
                "No users yet.",
            )?;
            output::hint(
                "Usernames and sign-in status need the Manage users permission; \
                log in again and allow it to see them.",
            );
            Ok(())
        }
    })
}

fn print_user(user: &Value) -> output::Details {
    let account = &user["account"];
    output::Details::new(output::field(user, "displayName"))
        .field("ID", output::field(user, "principalId"))
        .field("Username", output::field(account, "username"))
        .field("Email", output::field(user, "email"))
        .state(
            "Status",
            match account["principal"]["enabled"].as_bool() {
                Some(true) => "enabled",
                Some(false) => "disabled",
                None => "",
            },
        )
        .field(
            "Password",
            match account["hasLocalPassword"].as_bool() {
                Some(true) => "set",
                Some(false) => "not set",
                None => "",
            },
        )
        .field("Revision", output::field(account, "principal.revision"))
}

async fn users_show_command(format: OutputFormat, args: &UserRefArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let user = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .users_get(&auth_types::AuthUserGetRequest {
            user_id: wire(&args.user_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    output::emit(format, &user, |user| {
        print_user(user).print();
        Ok(())
    })
}

/// An OIDC sign-in from `--provider` and `--subject`, asking for whichever is
/// missing: the provider from the configured ones, the subject as typed.
async fn oidc_sign_in(
    form: &mut crate::prompt::Form,
    auth: &trellis_runtime_apis::apps::trellis_cli::trellis_auth_v1::Client,
    args: &UserIdentityArgs,
) -> miette::Result<(String, String)> {
    let provider = if args.provider.is_none() && form.is_interactive() {
        let page = serde_json::to_value(
            auth.oidc_providers_list(&auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor: None,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        )
        .into_diagnostic()?;
        let choices = output::list(&page, "items")
            .iter()
            .map(|provider| {
                (
                    output::field(provider, "providerId"),
                    format!(
                        "{} ({})",
                        output::field(provider, "title"),
                        output::field(provider, "issuer")
                    ),
                )
            })
            .collect::<Vec<_>>();
        miette::ensure!(
            !choices.is_empty(),
            help = "Add one with `trellis sign-in providers add`.",
            "no identity providers are set up"
        );
        form.choose("Identity provider", "--provider", None, &choices, None)?
    } else {
        form.required(
            "Identity provider ID",
            "--provider",
            args.provider.clone(),
            None,
            |_| Ok(()),
        )?
    };
    let subject = form.required(
        "The provider's subject (sub) for the user",
        "--subject",
        args.subject.clone(),
        None,
        |_| Ok(()),
    )?;
    Ok((provider, subject))
}

/// Show a user and the OIDC sign-ins linked to it.
fn print_user_with_identities(found: &Value) {
    print_user(&found["user"])
        .section("OIDC sign-ins")
        .items(output::list(found, "identities").iter().map(|identity| {
            format!(
                "{}  {}  {}",
                output::field(identity, "issuer"),
                output::field(identity, "subject"),
                output::relative_time(&output::field(identity, "linkedAt"))
            )
        }))
        .print();
}

async fn users_find_command(format: OutputFormat, args: &UserIdentityArgs) -> miette::Result<()> {
    let mut form = crate::prompt::Form::new(args.provider.is_none() || args.subject.is_none());
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let (provider, subject) = oidc_sign_in(&mut form, &auth, args).await?;
    form.finish()?;
    let found = auth
        .users_find(&auth_types::AuthUserFindRequest {
            user_id: None,
            identity: Some(auth_types::AuthUserFindIdentity {
                provider_id: wire(&provider)?,
                subject,
                extra: Default::default(),
            }),
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .found
        .ok_or_else(|| miette::miette!("no user has that sign-in"))?;
    output::emit(format, &found, |found| {
        print_user_with_identities(found);
        Ok(())
    })
}

/// Link (`unlink` false) or unlink an OIDC sign-in for a user, at the
/// account's current revision.
async fn users_identity_command(
    format: OutputFormat,
    args: &UserIdentityChangeArgs,
    unlink: bool,
) -> miette::Result<()> {
    let mut form = crate::prompt::Form::new(
        args.identity.provider.is_none() || args.identity.subject.is_none(),
    );
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let (provider, subject) = oidc_sign_in(&mut form, &auth, &args.identity).await?;
    form.finish()?;
    let current = auth
        .users_get(&auth_types::AuthUserGetRequest {
            user_id: wire(&args.user_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    let account = current
        .account
        .ok_or_else(|| miette::miette!("account management authority is required"))?;
    form.confirm(
        &format!(
            "{} an OIDC sign-in {} {}",
            if unlink { "Unlink" } else { "Link" },
            if unlink { "from" } else { "to" },
            current.display_name
        ),
        &[("Provider", provider.clone()), ("Subject", subject.clone())],
    )?;
    let identity = auth_types::AuthUserFindIdentity {
        provider_id: wire(&provider)?,
        subject: subject.clone(),
        extra: Default::default(),
    };
    let found = if unlink {
        auth.users_identities_unlink(&auth_types::AuthUserIdentitiesUnlinkRequest {
            user_id: current.principal_id,
            identity,
            expected_revision: account.principal.revision,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
    } else {
        auth.users_identities_link(&auth_types::AuthUserIdentitiesLinkRequest {
            user_id: current.principal_id,
            identity,
            expected_revision: account.principal.revision,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
    };
    output::emit(format, &found, |found| {
        output::success(format!(
            "{} {} {} {}",
            if unlink { "Unlinked" } else { "Linked" },
            output::strong(&subject),
            if unlink { "from" } else { "to" },
            output::field(&found["user"], "displayName")
        ));
        print_user_with_identities(found);
        Ok(())
    })
}

async fn users_create_command(format: OutputFormat, args: &UserCreateArgs) -> miette::Result<()> {
    // An OIDC sign-in is optional, but naming either half asks for the other.
    let linking = args.identity.provider.is_some() || args.identity.subject.is_some();
    let mut form = crate::prompt::Form::new(
        args.username.is_none()
            || (linking && (args.identity.provider.is_none() || args.identity.subject.is_none())),
    );
    let username = form
        .required(
            "Username",
            "--username",
            args.username.clone(),
            None,
            |_| Ok(()),
        )?
        .trim()
        .to_owned();
    let name = form.optional(
        "Name shown for the user (empty to use the username)",
        args.name.clone(),
        String::new(),
        |_| Ok(()),
    )?;
    let connection = if linking {
        let (_selection, connected) = connect_authenticated_cli_client().await?;
        Some(connected)
    } else {
        None
    };
    let identity = match &connection {
        Some(connected) => {
            let auth =
                trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
                    .trellis_auth_v1();
            Some(oidc_sign_in(&mut form, &auth, &args.identity).await?)
        }
        None => None,
    };
    form.finish()?;
    if username.is_empty() {
        return Err(miette::miette!("--username must not be empty"));
    }
    let name = trimmed_optional(&Some(name)).unwrap_or_else(|| username.clone());
    let mut summary = vec![("Name", name.clone())];
    if let Some((provider, subject)) = &identity {
        summary.push(("Provider", provider.clone()));
        summary.push(("Subject", subject.clone()));
    }
    form.confirm(&format!("Create user {username}"), &summary)?;
    let connected = match connection {
        Some(connected) => connected,
        None => connect_authenticated_cli_client().await?.1,
    };
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let user = auth
        .users_create(&auth_types::AuthUserCreateRequest {
            given_name: None,
            family_name: None,
            email: None,
            display_name: wire(name)?,
            username: wire(username)?,
            password: None,
            identities: identity
                .as_ref()
                .map(|(provider, subject)| {
                    Ok::<_, miette::Report>(vec![auth_types::AuthUserFindIdentity {
                        provider_id: wire(provider)?,
                        subject: subject.clone(),
                        extra: Default::default(),
                    }])
                })
                .transpose()?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    // A person who signs in through their provider needs no password.
    let setup_flow = if identity.is_some() {
        None
    } else {
        Some(
            auth.users_password_reset_create(&auth_types::AuthPasswordResetCreateRequest {
                user_id: user.principal_id,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        )
    };
    output::emit(
        format,
        &json!({ "user": user, "setupFlow": setup_flow }),
        |value| {
            output::success(format!(
                "Created user {}",
                output::strong(output::field(&value["user"], "account.username"))
            ));
            let setup = &value["setupFlow"];
            if setup.is_null() {
                output::Details::untitled()
                    .field("ID", output::field(&value["user"], "principalId"))
                    .print();
                return Ok(());
            }
            output::Details::untitled()
                .field("ID", output::field(&value["user"], "principalId"))
                .field("Setup code", output::field(setup, "token"))
                .print();
            output::not_kept(
                "setup code",
                &format!(
                    "Send it to the person; it lets them choose a password until {}.",
                    output::absolute_time(&output::field(setup, "expiresAt"))
                ),
            );
            Ok(())
        },
    )
}

async fn users_edit_command(format: OutputFormat, args: &UserEditArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let current = auth
        .users_get(&auth_types::AuthUserGetRequest {
            user_id: wire(&args.user_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    let account = current
        .account
        .ok_or_else(|| miette::miette!("account management authority is required"))?;
    let user = auth
        .users_update(&auth_types::AuthUserUpdateRequest {
            given_name: None,
            family_name: None,
            email: None,
            clear_email: None,
            display_name: trimmed_optional(&args.name).map(wire).transpose()?,
            expected_profile_revision: args.name.as_ref().map(|_| account.profile_revision),
            enabled: if args.active {
                true
            } else if args.inactive {
                false
            } else {
                account.principal.enabled
            },
            expected_revision: account.principal.revision,
            user_id: current.principal_id,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    output::emit(
        format,
        &json!({ "user": user, "userId": args.user_id }),
        |value| {
            output::success(format!(
                "Updated user {}",
                output::strong(output::field(&value["user"], "displayName"))
            ));
            print_user(&value["user"]).print();
            Ok(())
        },
    )
}

async fn users_delete_command(format: OutputFormat, args: &UserRefArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let current = auth
        .users_get(&auth_types::AuthUserGetRequest {
            user_id: wire(&args.user_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    let account = current
        .account
        .ok_or_else(|| miette::miette!("account management authority is required"))?;
    auth.users_delete(&auth_types::AuthUserDeleteRequest {
        user_id: current.principal_id,
        expected_revision: account.principal.revision,
        idempotency_key: wire(cli_idempotency_key())?,
        extra: Default::default(),
    })
    .await
    .into_diagnostic()?;
    output::emit(
        format,
        &json!({ "deleted": true, "userId": args.user_id }),
        |_| {
            output::success(format!(
                "Deleted user {} and signed them out everywhere",
                output::strong(&current.display_name)
            ));
            Ok(())
        },
    )
}

fn print_role(role: &Value, members: Option<&[Value]>) {
    let mut details = output::Details::new(output::field(role, "title"))
        .field("ID", output::field(role, "roleId"))
        .field("Description", output::field(role, "description"))
        .field(
            "New users",
            if role["assignToNewUsers"] == json!(true) {
                "get it automatically"
            } else {
                "no"
            },
        )
        .field("Revision", output::field(role, "revision"))
        .section("Capabilities")
        .items(
            output::list(role, "capabilities")
                .iter()
                .map(|capability| output::field(capability, "capabilityId")),
        );
    if let Some(members) = members {
        details = details
            .section("Members")
            .items(members.iter().map(|member| output::field(member, "")));
    }
    details.print();
}

async fn roles_list_command(format: OutputFormat) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let mut roles = Vec::new();
    let mut cursor = None;
    loop {
        let page = auth
            .roles_list(&auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?;
        roles.extend(page.items);
        match page.page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    output::emit(format, &json!({ "items": roles }), |value| {
        let rows = output::list(value, "items")
            .iter()
            .map(|role| {
                json!({
                    "roleId": role["roleId"],
                    "title": role["title"],
                    "capabilities": output::list(role, "capabilities").len(),
                    "newUsers": role["assignToNewUsers"],
                })
            })
            .collect::<Vec<_>>();
        output::table(
            &rows,
            &[
                output::muted("ID", "roleId"),
                output::col("Name", "title"),
                output::col("Capabilities", "capabilities"),
                output::col("New users", "newUsers"),
            ],
            "No roles yet.",
        )
    })
}

async fn roles_show_command(format: OutputFormat, args: &RoleRefArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let role_id = wire(&args.role_id)?;
    let role = auth
        .roles_get(&auth_types::AuthRoleGetRequest {
            role_id,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .role;
    let members = current_assignments(&auth, &role, None)
        .await?
        .into_iter()
        .map(|assignment| assignment.principal_id)
        .collect::<Vec<_>>();
    output::emit(
        format,
        &json!({ "role": role, "members": members }),
        |value| {
            print_role(&value["role"], Some(output::list(value, "members")));
            Ok(())
        },
    )
}

/// The role's assignments, on every page, that belong to its current identity.
/// Deleting and recreating a role keeps the earlier assignments, which no
/// longer give the role and cannot be revoked.
async fn current_assignments(
    auth: &trellis_runtime_apis::apps::trellis_cli::trellis_auth_v1::Client,
    role: &auth_types::AuthRole,
    principal_id: Option<trellis_protocol::model::Ulid>,
) -> miette::Result<Vec<auth_types::AuthRoleAssignment>> {
    let mut assignments = Vec::new();
    let mut cursor = None;
    loop {
        let page = auth
            .role_assignments_list(&auth_types::AuthRoleAssignmentsListRequest {
                principal_id,
                role_id: Some(role.role_id),
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?;
        assignments.extend(
            page.items
                .into_iter()
                .filter(|item| item.role_identity_generation == role.identity_generation),
        );
        match page.page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(assignments),
        }
    }
}

fn print_saved_role(
    format: OutputFormat,
    role: auth_types::AuthRole,
    verb: &str,
) -> miette::Result<()> {
    output::emit(format, &json!({ "role": role }), |value| {
        output::success(format!(
            "{verb} role {}",
            output::strong(output::field(&value["role"], "title"))
        ));
        print_role(&value["role"], None);
        Ok(())
    })
}

async fn roles_create_command(format: OutputFormat, args: &RoleCreateArgs) -> miette::Result<()> {
    let mut form = crate::prompt::Form::new(args.title.is_none());
    let title = form.required(
        "Role title",
        "--title",
        args.title.clone(),
        None,
        |_| Ok(()),
    )?;
    let description = form.optional(
        "Description (optional)",
        args.description.clone(),
        String::new(),
        |_| Ok(()),
    )?;
    form.explain(
        "Capabilities the role gives, such as trellis.auth.v1:users.read. \
        Separate several with spaces; `trellis apis get <API>` lists an API's.",
    );
    let capabilities = form.list(
        "Capabilities (optional)",
        "--capability",
        args.capabilities.clone(),
        Vec::new(),
        false,
        |_| Ok(()),
    )?;
    let new_users = form.yes_no(
        "Give this role to every user created from now on?",
        crate::prompt::pair(args.new_users, args.no_new_users),
        false,
    )?;
    form.finish()?;
    form.confirm(
        &format!("Create role {title}"),
        &[
            ("Description", description.clone()),
            ("Capabilities", capabilities.join(" ")),
            ("New users", if new_users { "yes" } else { "no" }.to_owned()),
        ],
    )?;
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let role = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .roles_put(&auth_types::AuthRolePutRequest {
            role_id: None,
            expected_revision: 0u64.into(),
            idempotency_key: wire(cli_idempotency_key())?,
            title: wire(title.trim())?,
            description: wire(&description)?,
            capability_ids: wire(&capabilities)?,
            assign_to_new_users: new_users,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .role;
    print_saved_role(format, role, "Created")
}

async fn roles_edit_command(format: OutputFormat, args: &RoleEditArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let current = auth
        .roles_get(&auth_types::AuthRoleGetRequest {
            role_id: wire(&args.role_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .role;
    let capability_ids = match &args.capabilities {
        Some(capabilities) => wire(capabilities)?,
        None => current
            .capabilities
            .iter()
            .map(|capability| capability.capability_id.clone())
            .collect(),
    };
    let role = auth
        .roles_put(&auth_types::AuthRolePutRequest {
            role_id: Some(current.role_id),
            expected_revision: current.revision.0 .0.into(),
            idempotency_key: wire(cli_idempotency_key())?,
            title: wire(args.title.as_deref().unwrap_or(&current.title).trim())?,
            description: wire(args.description.as_deref().unwrap_or(&current.description))?,
            capability_ids,
            assign_to_new_users: if args.new_users {
                true
            } else if args.no_new_users {
                false
            } else {
                current.assign_to_new_users
            },
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .role;
    print_saved_role(format, role, "Updated")
}

async fn roles_delete_command(format: OutputFormat, args: &RoleRefArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let current = auth
        .roles_get(&auth_types::AuthRoleGetRequest {
            role_id: wire(&args.role_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .role;
    auth.roles_delete(&auth_types::AuthRoleDeleteRequest {
        role_id: current.role_id,
        expected_revision: current.revision,
        idempotency_key: wire(cli_idempotency_key())?,
        extra: Default::default(),
    })
    .await
    .into_diagnostic()?;
    output::emit(format, &json!({ "roleId": current.role_id }), |_| {
        output::success(format!(
            "Deleted role {} and took it away from everyone who had it",
            output::strong(&current.title)
        ));
        Ok(())
    })
}

async fn roles_assign_command(format: OutputFormat, args: &RoleMemberArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let assignment = auth
        .role_assignments_assign(&auth_types::AuthRoleAssignRequest {
            principal_id: wire(&args.user_id)?,
            role_id: wire(&args.role_id)?,
            expected_revision: 0u64.into(),
            idempotency_key: wire(cli_idempotency_key())?,
            expires_at: None,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .assignment;
    if output::is_json(format) {
        return output::print_json(&json!({ "assignment": assignment }));
    }
    let role = auth
        .roles_get(&auth_types::AuthRoleGetRequest {
            role_id: assignment.role_id,
            extra: Default::default(),
        })
        .await
        .map(|response| response.role.title)
        .unwrap_or_else(|_| args.role_id.clone());
    output::success(format!(
        "Gave role {} to user {}",
        output::strong(role),
        output::strong(&args.user_id)
    ));
    Ok(())
}

async fn roles_revoke_command(format: OutputFormat, args: &RoleMemberArgs) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let role = auth
        .roles_get(&auth_types::AuthRoleGetRequest {
            role_id: wire(&args.role_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .role;
    let assignment = current_assignments(&auth, &role, Some(wire(&args.user_id)?))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            miette::miette!("user {} does not have role {}", args.user_id, args.role_id)
        })?;
    auth.role_assignments_revoke(&auth_types::AuthRoleRevokeRequest {
        assignment_id: assignment.assignment_id,
        expected_revision: assignment.revision,
        idempotency_key: wire(cli_idempotency_key())?,
        extra: Default::default(),
    })
    .await
    .into_diagnostic()?;
    output::emit(
        format,
        &json!({ "assignmentId": assignment.assignment_id }),
        |_| {
            output::success(format!(
                "Took role {} away from user {}",
                output::strong(&role.title),
                output::strong(&args.user_id)
            ));
            Ok(())
        },
    )
}

/// Writes the login URL atomically so a detached/nohup caller can read it.
fn write_login_url_file(path: &std::path::Path, login_url: &str) -> miette::Result<()> {
    use std::io::Write as _;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent).into_diagnostic()?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).into_diagnostic()?;
    temporary
        .write_all(login_url.as_bytes())
        .into_diagnostic()?;
    temporary.write_all(b"\n").into_diagnostic()?;
    temporary.flush().into_diagnostic()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .into_diagnostic()?;
    Ok(())
}

async fn login_command(format: OutputFormat, args: &LoginArgs) -> miette::Result<()> {
    let (server, root_certificates) = super::trust::discover(
        format,
        &args.trellis_url,
        args.ca_certificate.as_deref(),
        args.ca_fingerprint.as_deref(),
    )
    .await?;
    let selection = super::native_auth::Selection {
        scope: authlib::NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id,
            account: server.trust.account,
            client_id: "trellis.cli".into(),
            credential_handle: Ulid::generate(),
        },
        nats_servers: server.nats_servers,
        issuer_key: server.trust.issuer,
        rotation_sequence: server.trust.rotation_sequence,
        root_certificates,
        nats_sentinel: server.trust.nats_sentinel,
    };
    type Request = trellis_runtime_apis::apps::trellis_cli::Request;
    let challenge = selection
        .open()
        .await?
        .start_device(&json!([{
            "type": "trellis_capabilities",
            "required": Request::REQUIRED_CAPABILITIES,
            "optional": Request::OPTIONAL_CAPABILITIES,
        }]))
        .await
        .into_diagnostic()?;
    let login_url = challenge
        .verification_uri_complete()
        .unwrap_or(challenge.verification_uri())
        .to_owned();

    if let Some(path) = &args.login_url_file {
        write_login_url_file(path, &login_url)?;
    }

    if output::is_json(format) {
        let mut pending = pending_agent_login_json(&login_url);
        pending["userCode"] = json!(challenge.user_code());
        output::print_json_progress(&pending)?;
    } else {
        output::note(render_agent_login_instructions(
            &login_url,
            challenge.user_code(),
        )?);
        output::hint("Waiting for you to approve the sign-in...");
    }
    let session = challenge.complete().await.into_diagnostic()?;
    let principal_id = session.admission.credential.principal_id.clone();
    let connected =
        trellis_rs::generated::Client::connect_user(trellis_rs::client::UserConnectOptions::new(
            selection.nats_servers.clone(),
            10_000,
            session,
        ))
        .await
        .into_diagnostic()?;
    selection.save()?;
    if output::is_json(format) {
        return output::print_json(&json!({ "principalId": principal_id, "loggedIn": true }));
    }
    // The name needs an optional permission the person may have declined.
    let server = output::strong(&selection.scope.issuer);
    match current_user(&connected).await {
        Ok(me) => output::success(format!(
            "Logged in to {server} as {}",
            output::strong(me.principal.title)
        )),
        Err(_) => output::success(format!("Logged in to {server}")),
    }
    Ok(())
}

async fn logout_command(format: OutputFormat) -> miette::Result<()> {
    let mut revoked = false;
    let mut revoke_error = None;
    if super::native_auth::Selection::load().is_ok() {
        match connect_authenticated_cli_client().await {
            Ok((_selection, connected)) => match revoke_current_session(&connected).await {
                Ok(()) => revoked = true,
                Err(error) => revoke_error = Some(error.to_string()),
            },
            Err(error) => revoke_error = Some(error.to_string()),
        }
    }
    let removed = super::native_auth::Selection::clear()?;
    let mut response = json!({ "cleared": removed, "revoked": revoked });
    if let Some(error) = &revoke_error {
        response["revokeError"] = Value::String(error.clone());
    }
    output::emit(format, &response, |_| {
        if !removed {
            output::unchanged("Not logged in");
        } else if revoked {
            output::success("Logged out and ended the sign-in on the server");
        } else {
            output::success("Logged out on this computer");
            if let Some(error) = &revoke_error {
                output::warning(format!(
                    "The server didn't end the sign-in ({error}); it stays valid until it expires \
                    or an administrator revokes it."
                ));
            }
        }
        Ok(())
    })
}

pub(super) async fn current_user(
    connected: &trellis_rs::generated::Client,
) -> miette::Result<auth_types::AuthSessionsMeResponse> {
    trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1()
        .sessions_me(&auth_types::AuthEmpty {
            extra: Default::default(),
        })
        .await
        .into_diagnostic()
}

async fn revoke_current_session(connected: &trellis_rs::generated::Client) -> miette::Result<()> {
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1();
    auth.sessions_logout(&auth_types::AuthEmpty {
        extra: Default::default(),
    })
    .await
    .into_diagnostic()?;
    Ok(())
}

async fn status_command(format: OutputFormat) -> miette::Result<()> {
    let (selection, connected) = connect_authenticated_cli_client().await?;
    let me = current_user(&connected).await?;
    let mut response = serde_json::to_value(&me).into_diagnostic()?;
    response["loggedIn"] = Value::Bool(true);
    output::emit(format, &response, |me| {
        let login = &me["login"];
        let method = match output::field(login, "method").as_str() {
            "local" => "username and password",
            "oidc" => "identity provider",
            other => other,
        }
        .to_owned();
        output::Details::new(output::field(me, "principal.title"))
            .field("ID", output::field(me, "principal.principalId"))
            .field("Kind", output::field(me, "principal.kind"))
            .field("Server", &selection.scope.issuer)
            .field("Signed in with", method)
            .time("Since", &output::field(login, "createdAt"))
            .time("Expires", &output::field(login, "expiresAt"))
            .print();
        Ok(())
    })
}

async fn issuers_revoke_command(
    format: OutputFormat,
    args: &IssuersRevokeArgs,
) -> miette::Result<()> {
    // Runs that reuse an admission share one session, which may hold authority
    // signed by the key being revoked and would end with it, mid-command.
    let (_state, connected) = crate::app::connect_cli_client(false).await?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .issuers_revoke(&auth_types::AuthIssuerRevokeRequest {
            idempotency_key: wire(cli_idempotency_key())?,
            key_id: wire(&args.key_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    output::emit(format, &response, |response| {
        output::success(format!(
            "Revoked issuer key {}",
            output::strong(output::field(response, "keyId"))
        ));
        print_enforcement(&response["enforcement"]);
        Ok(())
    })
}
