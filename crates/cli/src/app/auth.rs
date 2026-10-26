use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::*;
use crate::output;
use miette::IntoDiagnostic;
use qrcode::{render::unicode, QrCode};
use serde_json::{json, Value};
use trellis_rs::auth as authlib;
use trellis_rs::generated::AppRequestDescriptor;
use trellis_runtime_apis::types as auth_types;
use ulid::Ulid;

pub(crate) fn render_agent_login_instructions(login_url: &str) -> miette::Result<String> {
    let qr = QrCode::new(login_url.as_bytes()).into_diagnostic()?;
    let qr = qr.render::<unicode::Dense1x2>().quiet_zone(false).build();
    Ok(format!(
        "Open this activation URL:\n{login_url}\n\nScan this QR code:\n{qr}"
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
        UsersSubcommand::Create(args) => users_create_command(format, &args).await,
        UsersSubcommand::Edit(args) => users_edit_command(format, &args).await,
    }
}

pub(super) async fn clients(command: ClientsCommand) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let result = match command {
        ClientsCommand::List { cursor } => serde_json::to_value(
            auth.o_auth_clients_list(&auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        ),
        ClientsCommand::Get { client_id } => serde_json::to_value(
            auth.o_auth_clients_get(&auth_types::AuthOAuthClientGetRequest {
                client_id: wire(client_id)?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        ),
        ClientsCommand::Put { input } => {
            let request: auth_types::AuthOAuthClientPutRequest =
                serde_json::from_slice(&std::fs::read(input).into_diagnostic()?)
                    .into_diagnostic()?;
            serde_json::to_value(auth.o_auth_clients_put(&request).await.into_diagnostic()?)
        }
        ClientsCommand::Disable(args) => serde_json::to_value(
            auth.o_auth_clients_disable(&auth_types::AuthOAuthClientMutationRequest {
                client_id: wire(args.client_id)?,
                expected_revision: wire(args.expected_revision.to_string())?,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        ),
        ClientsCommand::Delete(args) => serde_json::to_value(
            auth.o_auth_clients_delete(&auth_types::AuthOAuthClientMutationRequest {
                client_id: wire(args.client_id)?,
                expected_revision: wire(args.expected_revision.to_string())?,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        ),
    }
    .into_diagnostic()?;
    output::print_json(&result)
}

pub(super) async fn apis(command: ApisCommand) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let result = match command {
        ApisCommand::List { cursor } => serde_json::to_value(
            auth.apis_list(&auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        ),
        ApisCommand::Get { api_id } => serde_json::to_value(
            auth.apis_get(&auth_types::AuthApiGetRequest {
                api_id: wire(api_id)?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?,
        ),
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
            serde_json::to_value(json!({ "reviews": reviews }))
        }
        ApisCommand::Accept(args) => {
            let participant = crate::app::deploy::compile_participant_input(
                &args.source,
                args.participant.as_deref(),
                None,
            )?;
            let mut apis = Vec::new();
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
                        continue;
                    }
                }
                miette::ensure!(
                    review.compatible,
                    "API {api_id} changes incompatibly ({}); replace it with \
                    `trellis apis force-replace` using review {}",
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
            }
            serde_json::to_value(json!({ "apis": apis }))
        }
        ApisCommand::ForceReplace { input } => {
            let request: auth_types::AuthApiForceReplaceRequest =
                serde_json::from_slice(&std::fs::read(input).into_diagnostic()?)
                    .into_diagnostic()?;
            serde_json::to_value(auth.apis_force_replace(&request).await.into_diagnostic()?)
        }
    }
    .into_diagnostic()?;
    output::print_json(&result)
}

pub(super) async fn theme(command: ThemeCommand) -> miette::Result<()> {
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
    let result = match command {
        ThemeCommand::Show(_) => serde_json::to_value(current),
        ThemeCommand::Remove(args) => serde_json::to_value(
            auth.portal_themes_delete(&auth_types::AuthPortalThemeDeleteRequest {
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
            .into_diagnostic()?,
        ),
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
                        _ => miette::bail!("the logo must be a .png, .jpg, .webp, or .svg file"),
                    };
                    let bytes = std::fs::read(path).into_diagnostic()?;
                    Some(json!({
                        "format": format,
                        "base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
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
            serde_json::to_value(auth.portal_themes_put(&request).await.into_diagnostic()?)
        }
    }
    .into_diagnostic()?;
    output::print_json(&result)
}

fn trimmed_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn cli_idempotency_key() -> String {
    Ulid::new().to_string()
}

async fn users_list_command(format: OutputFormat) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let users = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .users_list(&auth_types::AuthPageRequest {
            search: None,
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(users).into_diagnostic()?)?;
    } else {
        let rows = users
            .items
            .iter()
            .map(|user| {
                vec![
                    user.principal_id.to_string(),
                    user.display_name.clone(),
                    user.account
                        .as_ref()
                        .map(|account| account.username.clone())
                        .unwrap_or_default(),
                    user.account
                        .as_ref()
                        .map(|account| account.principal.enabled.to_string())
                        .unwrap_or_default(),
                    user.account
                        .as_ref()
                        .map(|account| account.principal.revision.to_string())
                        .unwrap_or_default(),
                ]
            })
            .collect();
        println!(
            "{}",
            output::table(&["userId", "name", "username", "enabled", "revision"], rows)
        );
    }
    Ok(())
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
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(user).into_diagnostic()?)?;
    } else {
        output::print_info(&format!("userId={}", user.principal_id));
        output::print_info(&format!("name={}", user.display_name));
        if let Some(account) = user.account {
            output::print_info(&format!("username={}", account.username));
            output::print_info(&format!("enabled={}", account.principal.enabled));
            output::print_info(&format!("revision={}", account.principal.revision.0 .0));
        }
    }
    Ok(())
}

async fn users_create_command(format: OutputFormat, args: &UserCreateArgs) -> miette::Result<()> {
    let username = args.username.trim().to_owned();
    if username.is_empty() {
        return Err(miette::miette!("--username must not be empty"));
    }
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let user = auth
        .users_create(&auth_types::AuthUserCreateRequest {
            given_name: None,
            family_name: None,
            email: None,
            display_name: wire(trimmed_optional(&args.name).unwrap_or_else(|| username.clone()))?,
            username: wire(username)?,
            password: None,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .user;
    let setup_flow = auth
        .users_password_reset_create(&auth_types::AuthPasswordResetCreateRequest {
            user_id: user.principal_id,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&json!({ "user": user, "setupFlow": setup_flow }))?;
    } else {
        output::print_success("created user");
        output::print_info(&format!("userId={}", user.principal_id));
        output::print_info(&format!(
            "setupFlow={}",
            serde_json::to_string(&setup_flow).into_diagnostic()?
        ));
    }
    Ok(())
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
    if output::is_json(format) {
        output::print_json(&json!({ "user": user, "userId": args.user_id }))?;
    } else {
        output::print_success("updated user");
        output::print_info(&format!("userId={}", args.user_id));
    }
    Ok(())
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
            credential_handle: Ulid::new(),
        },
        nats_servers: server.nats_servers,
        issuer_key: server.trust.issuer,
        rotation_sequence: server.trust.rotation_sequence,
        root_certificates,
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
        output::print_info(&render_agent_login_instructions(&login_url)?);
        output::print_info(&format!("User code: {}", challenge.user_code()));
    }
    let session = challenge.complete().await.into_diagnostic()?;
    let principal_id = session.admission.credential.principal_id.clone();
    let _connected =
        trellis_rs::generated::Client::connect_user(trellis_rs::client::UserConnectOptions::new(
            selection.nats_servers.clone(),
            10_000,
            session,
        ))
        .await
        .into_diagnostic()?;
    selection.save()?;
    if output::is_json(format) {
        output::print_json(&json!({ "principalId": principal_id, "loggedIn": true }))?;
    } else {
        output::print_success("logged in native OAuth session");
        output::print_info(&format!("principalId={principal_id}"));
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
    if output::is_json(format) {
        let mut response = json!({ "cleared": removed, "revoked": revoked });
        if let Some(error) = &revoke_error {
            response["revokeError"] = Value::String(error.clone());
        }
        output::print_json(&response)?;
    } else if removed {
        if revoked {
            output::print_success("revoked remote login and cleared local selection");
        } else if let Some(error) = &revoke_error {
            output::print_success("cleared local OAuth selection");
            output::print_info(&format!(
                "warning: remote session revocation failed: {error}"
            ));
        } else {
            output::print_success("cleared local OAuth selection");
        }
    } else {
        output::print_info("no selected native OAuth credential");
    }
    Ok(())
}

pub(super) async fn current_user(
    connected: &trellis_rs::generated::Client,
) -> miette::Result<auth_types::AuthSessionsMeResponse> {
    trellis_runtime_apis::apis::trellis_auth_v1::Client::from_generated(connected.clone())
        .sessions_me(&auth_types::AuthEmpty {
            extra: Default::default(),
        })
        .await
        .into_diagnostic()
}

async fn revoke_current_session(connected: &trellis_rs::generated::Client) -> miette::Result<()> {
    let auth =
        trellis_runtime_apis::apis::trellis_auth_v1::Client::from_generated(connected.clone());
    auth.sessions_logout(&auth_types::AuthEmpty {
        extra: Default::default(),
    })
    .await
    .into_diagnostic()?;
    Ok(())
}

async fn status_command(format: OutputFormat) -> miette::Result<()> {
    let (_selection, connected) = connect_authenticated_cli_client().await?;
    let me = current_user(&connected).await?;

    if output::is_json(format) {
        let mut response = serde_json::to_value(&me).into_diagnostic()?;
        response["loggedIn"] = Value::Bool(true);
        output::print_json(&response)?;
    } else {
        output::print_success("native OAuth session is active");
        output::print_info(&format!("principalId={}", me.principal.principal_id));
    }

    Ok(())
}

async fn issuers_revoke_command(
    format: OutputFormat,
    args: &IssuersRevokeArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .issuers_revoke(&auth_types::AuthIssuerRevokeRequest {
            idempotency_key: wire(cli_idempotency_key())?,
            key_id: wire(&args.key_id)?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else {
        output::print_success("revoked issuer");
        output::print_info(&format!("keyId={}", response.key_id));
    }
    Ok(())
}
