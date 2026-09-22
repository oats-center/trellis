use crate::app::{connect_authenticated_cli_client, wire, wire_u64};
use crate::cli::*;
use crate::output;
use miette::IntoDiagnostic;
use qrcode::{render::unicode, QrCode};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use trellis_rs::auth as authlib;
use trellis_runtime_apis::apis::trellis_auth_v1::{
    rpc::ParticipantsGetError as AuthParticipantsGetError, Client as AuthClient,
};
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

fn authenticated_user_json(me: &authlib::AuthenticatedUser) -> Value {
    json!({
        "userId": &me.user_id,
        "principalId": &me.principal_id,
        "state": &me.state,
        "name": &me.name,
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

pub(super) async fn identity(format: OutputFormat, command: IdentityCommand) -> miette::Result<()> {
    match command.command {
        IdentitySubcommand::Grants(command) => match command.command {
            IdentityGrantsSubcommand::List(args) => {
                identity_grants_list_command(format, &args).await
            }
            IdentityGrantsSubcommand::Get(args) => identity_grants_get_command(format, &args).await,
            IdentityGrantsSubcommand::Set(args) => identity_grants_set_command(format, &args).await,
            IdentityGrantsSubcommand::Revoke(args) => {
                identity_grants_revoke_command(format, &args).await
            }
        },
    }
}

pub(super) async fn participants(
    format: OutputFormat,
    command: ParticipantsCommand,
) -> miette::Result<()> {
    match command.command {
        ParticipantsSubcommand::Install(args) => participants_install_command(format, &args).await,
    }
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

pub(super) async fn portals(format: OutputFormat, command: PortalsCommand) -> miette::Result<()> {
    portals_command(format, command).await
}

async fn portals_command(format: OutputFormat, command: PortalsCommand) -> miette::Result<()> {
    let command_name = match command.command {
        PortalsSubcommand::List => "portals list",
        PortalsSubcommand::Login(login) => match login.command {
            PortalsLoginSubcommand::Default => "portals login default",
            PortalsLoginSubcommand::Selection => "portals login selection",
        },
    };
    if output::is_json(format) {
        output::print_json(&json!({
            "status": "not_implemented",
            "command": command_name,
            "message": "Portal admin RPC client wiring is pending; use Console or call Auth.Portals.* RPCs directly."
        }))?;
    } else {
        output::print_info(&format!(
            "{command_name}: portal admin RPC client wiring is pending; use Console or call Auth.Portals.* RPCs directly."
        ));
    }
    Ok(())
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

fn identity_labels(identities: &[Value]) -> String {
    identities
        .iter()
        .filter_map(|identity| {
            let provider = identity.get("provider")?.as_str()?;
            let subject = identity.get("subject")?.as_str()?;
            Some(format!("{provider}:{subject}"))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn latest_last_auth_by_user(sessions: &[Value]) -> BTreeMap<String, String> {
    let mut last_auth_by_user: BTreeMap<String, String> = BTreeMap::new();
    for session in sessions {
        let Some(user_id) = session
            .get("principal")
            .and_then(|principal| principal.get("userId"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(last_auth) = session.get("lastAuth").and_then(Value::as_str) else {
            continue;
        };
        match last_auth_by_user.get(user_id) {
            Some(existing) if existing.as_str() >= last_auth => {}
            _ => {
                last_auth_by_user.insert(user_id.to_string(), last_auth.to_string());
            }
        }
    }
    last_auth_by_user
}

fn user_label(user: &Value) -> String {
    user.get("name")
        .and_then(Value::as_str)
        .or_else(|| user.get("email").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn user_email(user: &Value) -> String {
    user.get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn string_array_field(user: &Value, field: &str) -> Vec<String> {
    user.get(field)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn direct_capabilities(user: &Value) -> String {
    string_array_field(user, "capabilities").join(",")
}

fn capability_groups(user: &Value) -> String {
    string_array_field(user, "capabilityGroups").join(",")
}

fn identities_field(user: &Value) -> String {
    user.get("identities")
        .and_then(Value::as_array)
        .map(|identities| identity_labels(identities))
        .unwrap_or_default()
}

fn user_row(user: &Value, last_auth_by_user: &BTreeMap<String, String>) -> Vec<String> {
    let user_id = user
        .get("userId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    vec![
        user_id.clone(),
        user_label(user),
        user_email(user),
        user.get("active")
            .and_then(Value::as_bool)
            .map(|active| active.to_string())
            .unwrap_or_default(),
        direct_capabilities(user),
        capability_groups(user),
        identities_field(user),
        last_auth_by_user.get(&user_id).cloned().unwrap_or_default(),
    ]
}

async fn users_list_command(format: OutputFormat) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth_client = AuthClient::from_generated(connected.clone());
    let users = auth_client
        .users_list(&auth_types::AuthUsersListRequest {
            search: None,
            state: None,
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
        })
        .await
        .into_diagnostic()?;
    let user_values = users
        .items
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    let sessions = auth_client
        .sessions_list(&auth_types::AuthSessionsListRequest {
            principal_id: None,
            participant_id: None,
            state: None,
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
        })
        .await
        .map(|response| response.items)
        .unwrap_or_default();
    let session_values = sessions
        .iter()
        .filter_map(|session| serde_json::to_value(session).ok())
        .collect::<Vec<_>>();
    let last_auth_by_user = latest_last_auth_by_user(&session_values);

    if output::is_json(format) {
        output::print_json(&json!({
            "users": users.items,
            "lastAuthByUser": last_auth_by_user,
        }))?;
        return Ok(());
    }

    let rows = user_values
        .iter()
        .map(|user| user_row(user, &last_auth_by_user))
        .collect();
    println!(
        "{}",
        output::table(
            &[
                "userId",
                "label",
                "email",
                "active",
                "direct",
                "groups",
                "identities",
                "lastAuth"
            ],
            rows
        )
    );
    Ok(())
}

async fn users_show_command(format: OutputFormat, args: &UserRefArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth_client = AuthClient::from_generated(connected.clone());
    let user = auth_client
        .users_get(&auth_types::AuthUsersGetRequest {
            user_id: wire(&args.user_id)?,
        })
        .await
        .into_diagnostic()?
        .user;

    if output::is_json(format) {
        output::print_json(&json!({ "user": user }))?;
        return Ok(());
    }

    let user_value = serde_json::to_value(&user).into_diagnostic()?;
    output::print_info(&format!("userId={}", user.user_id));
    output::print_info(&format!("state={}", user.state));
    output::print_info(&format!(
        "name={}",
        wire::<Option<String>>(&user.name)?.as_deref().unwrap_or("")
    ));
    output::print_info(&format!(
        "email={}",
        wire::<Option<String>>(&user.email)?
            .as_deref()
            .unwrap_or("")
    ));
    output::print_info(&format!("direct={}", direct_capabilities(&user_value)));
    output::print_info(&format!("groups={}", capability_groups(&user_value)));
    output::print_info(&format!("identities={}", identities_field(&user_value)));
    Ok(())
}

async fn users_create_command(format: OutputFormat, args: &UserCreateArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth_client = AuthClient::from_generated(connected.clone());
    let username = trimmed_optional(&args.username)
        .ok_or_else(|| miette::miette!("--username is required to create a local user"))?;
    let user = auth_client
        .users_create(&auth_types::AuthUsersCreateRequest {
            email: wire(trimmed_optional(&args.email))?,
            name: wire(trimmed_optional(&args.name))?,
            image: wire(None::<String>)?,
            username: wire(Some(username))?,
            idempotency_key: wire(cli_idempotency_key())?,
        })
        .await
        .into_diagnostic()?
        .user;
    let setup_flow = auth_client
        .users_password_reset_create(&auth_types::AuthUsersPasswordResetCreateRequest {
            user_id: wire(&user.user_id)?,
            return_target: wire(None::<String>)?,
            idempotency_key: wire(cli_idempotency_key())?,
        })
        .await
        .into_diagnostic()?;

    if output::is_json(format) {
        output::print_json(&json!({
            "user": user,
            "setupFlow": setup_flow,
        }))?;
        return Ok(());
    }

    output::print_success("created user");
    output::print_info(&format!("userId={}", user.user_id));
    output::print_info(&format!(
        "setupFlow={}",
        serde_json::to_string(&setup_flow).into_diagnostic()?
    ));
    Ok(())
}

async fn users_edit_command(format: OutputFormat, args: &UserEditArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth_client = AuthClient::from_generated(connected.clone());
    let current = auth_client
        .users_get(&auth_types::AuthUsersGetRequest {
            user_id: wire(&args.user_id)?,
        })
        .await
        .into_diagnostic()?
        .user;
    let next_name = trimmed_optional(&args.name);
    let next_email = trimmed_optional(&args.email);

    let user = auth_client
        .users_update(&auth_types::AuthUsersUpdateRequest {
            email: wire(next_email.or(wire::<Option<String>>(&current.email)?))?,
            name: wire(next_name.or(wire::<Option<String>>(&current.name)?))?,
            image: current.image,
            state: if args.active {
                auth_types::AuthUsersUpdateRequestState::Active
            } else if args.inactive {
                auth_types::AuthUsersUpdateRequestState::Disabled
            } else {
                if current.state.to_string() == "active" {
                    auth_types::AuthUsersUpdateRequestState::Active
                } else {
                    auth_types::AuthUsersUpdateRequestState::Disabled
                }
            },
            user_id: wire(&args.user_id)?,
            expected_version: wire(current.version)?,
            idempotency_key: wire(cli_idempotency_key())?,
        })
        .await
        .into_diagnostic()?
        .user;

    if output::is_json(format) {
        output::print_json(&json!({
            "user": user,
            "userId": args.user_id,
        }))?;
        return Ok(());
    }

    output::print_success("updated user");
    output::print_info(&format!("userId={}", args.user_id));
    Ok(())
}

async fn login_command(format: OutputFormat, args: &LoginArgs) -> miette::Result<()> {
    let challenge = authlib::start_agent_login(&authlib::StartAgentLoginOpts {
        trellis_url: &args.trellis_url,
        participant_id: trellis_runtime_apis::participants::trellis_cli::PARTICIPANT_ID,
        allow_insecure_origin: args.allow_insecure_origin,
    })
    .await
    .into_diagnostic()?;
    let login_url = challenge.login_url().to_string();

    if output::is_json(format) {
        output::print_json_progress(&pending_agent_login_json(&login_url))?;
    } else {
        output::print_info(&render_agent_login_instructions(&login_url)?);
    }

    let outcome = challenge
        .complete(&args.trellis_url)
        .await
        .into_diagnostic()?;
    let state = outcome.state;
    let me = outcome.user;

    authlib::save_admin_session(&state).into_diagnostic()?;

    if output::is_json(format) {
        let mut response = authenticated_user_json(&me);
        response["sessionKey"] = Value::String(state.session_key().into_diagnostic()?);
        response["expiresAt"] = state.expires_at.map(Value::from).unwrap_or(Value::Null);
        output::print_json(&response)?;
    } else {
        output::print_success("logged in delegated agent session");
        output::print_info(&format!("userId={}", me.user_id));
        output::print_info(&format!("identity={}", me.principal_id));
        output::print_info(&format!("name={}", me.name.as_deref().unwrap_or("")));
        output::print_info(&format!(
            "sessionKey={}",
            state.session_key().into_diagnostic()?
        ));
        output::print_info(&format!("expiresAt={:?}", state.expires_at));
    }

    Ok(())
}

async fn logout_command(format: OutputFormat) -> miette::Result<()> {
    let mut revoked = false;
    let mut revoke_error = None;
    if let Ok(state) = authlib::load_admin_session() {
        match authlib::connect_admin_client_async(&state).await {
            Ok(connected) => match revoke_current_session(&connected).await {
                Ok(()) => revoked = true,
                Err(error) => revoke_error = Some(error.to_string()),
            },
            Err(error) => revoke_error = Some(error.to_string()),
        }
    }
    let removed = authlib::clear_admin_session().into_diagnostic()?;
    if output::is_json(format) {
        let mut response = json!({ "cleared": removed, "revoked": revoked });
        if let Some(error) = &revoke_error {
            response["revokeError"] = Value::String(error.clone());
        }
        output::print_json(&response)?;
    } else if removed {
        if revoked {
            output::print_success("revoked remote session and cleared local agent session");
        } else if let Some(error) = &revoke_error {
            output::print_success("cleared stored agent session");
            output::print_info(&format!(
                "warning: remote session revocation failed: {error}"
            ));
        } else {
            output::print_success("cleared stored agent session");
        }
    } else {
        output::print_info("no stored agent session found");
    }
    Ok(())
}

pub(super) async fn current_user(
    connected: &trellis_rs::generated::Client,
) -> Result<authlib::AuthenticatedUser, authlib::TrellisAuthError> {
    let response = AuthClient::from_generated(connected.clone())
        .sessions_me(&auth_types::AuthSessionsMeRequest {})
        .await
        .map_err(|error| authlib::TrellisAuthError::OperationFailed(error.to_string()))?;
    let user = wire::<Option<auth_types::AuthUsersGetResponseuser>>(response.user)
        .map_err(|error| authlib::TrellisAuthError::OperationFailed(error.to_string()))?
        .ok_or_else(|| {
            authlib::TrellisAuthError::NotUserSession(
                response.connection.principal_kind.as_str().to_owned(),
            )
        })?;
    Ok(serde_json::from_value(serde_json::to_value(user)?)?)
}

async fn revoke_current_session(
    connected: &trellis_rs::generated::Client,
) -> Result<(), authlib::TrellisAuthError> {
    let auth = AuthClient::from_generated(connected.clone());
    auth.sessions_logout(&auth_types::AuthSessionsLogoutRequest {})
        .await
        .map_err(|error| authlib::TrellisAuthError::OperationFailed(error.to_string()))?;
    Ok(())
}

async fn status_command(format: OutputFormat) -> miette::Result<()> {
    let (state, connected) = connect_authenticated_cli_client().await?;
    let me = current_user(&connected).await.into_diagnostic()?;

    if output::is_json(format) {
        let mut response = authenticated_user_json(&me);
        response["loggedIn"] = Value::Bool(true);
        response["sessionKey"] = Value::String(state.session_key().into_diagnostic()?);
        response["expiresAt"] = state.expires_at.map(Value::from).unwrap_or(Value::Null);
        output::print_json(&response)?;
    } else {
        output::print_success("delegated agent session is active");
        output::print_info(&format!("userId={}", me.user_id));
        output::print_info(&format!("identity={}", me.principal_id));
        output::print_info(&format!("name={}", me.name.as_deref().unwrap_or("")));
        output::print_info(&format!(
            "sessionKey={}",
            state.session_key().into_diagnostic()?
        ));
        output::print_info(&format!("expiresAt={:?}", state.expires_at));
    }

    Ok(())
}

async fn identity_grants_list_command(
    format: OutputFormat,
    args: &IdentityGrantsListArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let owner_id = match &args.user {
        Some(user) => user.clone(),
        None => current_user(&connected).await.into_diagnostic()?.user_id,
    };
    let response = AuthClient::from_generated(connected.clone())
        .grants_list(&auth_types::AuthGrantsListRequest {
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            owner_id: Some(wire(&owner_id)?),
            owner_kind: Some(auth_types::AuthGrantsListRequestOwnerKind::User),
            participant_id: args.participant.as_ref().map(wire).transpose()?,
            state: None,
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else {
        output::print_info(&format!("user={owner_id}"));
        output::print_info(&format!("matched grants={}", response.items.len()));
        let rows = response
            .items
            .iter()
            .map(|entry| {
                vec![
                    entry.owner_id.to_string(),
                    entry.participant_id.to_string(),
                    entry.state.to_string(),
                    entry.revision.to_string(),
                    entry.installed_revision.to_string(),
                ]
            })
            .collect();
        println!(
            "{}",
            output::table(
                &["owner", "participant", "state", "revision", "installed"],
                rows
            )
        );
    }
    Ok(())
}

async fn identity_grants_revoke_command(
    format: OutputFormat,
    args: &IdentityGrantsRevokeArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let owner_id = match &args.user {
        Some(user) => user.clone(),
        None => current_user(&connected).await.into_diagnostic()?.user_id,
    };
    let auth = AuthClient::from_generated(connected.clone());
    let expected_revision = match args.expected_revision {
        Some(revision) => revision,
        None => current_grant_revision(&connected, &owner_id, &args.participant_id)
            .await?
            .ok_or_else(|| miette::miette!("grant binding does not exist"))?,
    };
    let response = auth
        .grants_revoke(&auth_types::AuthGrantsRevokeRequest {
            expected_revision: wire(expected_revision.to_string())?,
            idempotency_key: wire(cli_idempotency_key())?,
            owner_id: wire(&owner_id)?,
            owner_kind: auth_types::AuthGrantsRevokeRequestOwnerKind::User,
            participant_id: wire(&args.participant_id)?,
            reason: wire(&args.reason)?,
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else {
        output::print_success("revoked identity grant");
        output::print_info(&format!("user={owner_id}"));
        output::print_info(&format!("participant={}", args.participant_id));
    }
    Ok(())
}

async fn identity_grants_get_command(
    format: OutputFormat,
    args: &IdentityGrantsGetArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let owner_id = match &args.user {
        Some(user) => user.clone(),
        None => current_user(&connected).await.into_diagnostic()?.user_id,
    };
    let response = AuthClient::from_generated(connected.clone())
        .grants_get(&auth_types::AuthGrantsGetRequest {
            owner_id: wire(&owner_id)?,
            owner_kind: auth_types::AuthGrantsGetRequestOwnerKind::User,
            participant_id: wire(&args.participant_id)?,
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else if let Some(binding) = wire::<Option<auth_types::AuthGrantBinding>>(response.binding)? {
        output::print_json(&binding)?;
    } else {
        output::print_info("no matching identity grant");
    }
    Ok(())
}

async fn identity_grants_set_command(
    format: OutputFormat,
    args: &IdentityGrantsSetArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let mut input: Value =
        serde_json::from_slice(&std::fs::read(&args.input).into_diagnostic()?).into_diagnostic()?;
    let object = input
        .as_object_mut()
        .ok_or_else(|| miette::miette!("grant input must be a JSON object"))?;
    let expected = [
        "expiresAt",
        "grants",
        "installedRevision",
        "platformPrivileges",
    ];
    if object.len() != expected.len() || expected.iter().any(|field| !object.contains_key(*field)) {
        return Err(miette::miette!(
            "grant input must contain exactly installedRevision, grants, platformPrivileges, and expiresAt"
        ));
    }
    let expected_revision = match args.expected_revision {
        Some(revision) => revision,
        None => current_grant_revision(&connected, &args.user, &args.participant_id)
            .await?
            .unwrap_or(0),
    };
    object.insert("ownerKind".to_owned(), json!("user"));
    object.insert("ownerId".to_owned(), json!(args.user));
    object.insert("participantId".to_owned(), json!(args.participant_id));
    object.insert("expectedRevision".to_owned(), json!(expected_revision));
    object.insert("idempotencyKey".to_owned(), json!(cli_idempotency_key()));
    let request: auth_types::AuthGrantsSetRequest =
        serde_json::from_value(input).into_diagnostic()?;
    let response = AuthClient::from_generated(connected.clone())
        .grants_set(&request)
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else {
        output::print_success("set identity grant");
        output::print_info(&format!("user={}", args.user));
        output::print_info(&format!("participant={}", args.participant_id));
    }
    Ok(())
}

async fn current_grant_revision(
    connected: &trellis_rs::generated::Client,
    owner_id: &str,
    participant_id: &str,
) -> miette::Result<Option<u64>> {
    let response = AuthClient::from_generated(connected.clone())
        .grants_get(&auth_types::AuthGrantsGetRequest {
            owner_id: wire(owner_id)?,
            owner_kind: auth_types::AuthGrantsGetRequestOwnerKind::User,
            participant_id: wire(participant_id)?,
        })
        .await
        .into_diagnostic()?;
    let binding = wire::<Option<auth_types::AuthGrantBinding>>(response.binding)?;
    Ok(binding.and_then(|binding| {
        serde_json::to_value(binding)
            .ok()?
            .get("revision")?
            .as_str()?
            .parse()
            .ok()
    }))
}

async fn participants_install_command(
    format: OutputFormat,
    args: &ParticipantsInstallArgs,
) -> miette::Result<()> {
    let participant = super::deploy::compile_participant_input(
        &args.source,
        args.participant.as_deref(),
        None,
        &[],
        &[],
    )?;
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth = AuthClient::from_generated(connected.clone());
    let expected_revision = match args.expected_revision {
        Some(revision) => revision,
        None => match auth
            .participants_get(&auth_types::AuthParticipantsGetRequest {
                participant_id: wire(&participant.participant_id)?,
                revision: None,
            })
            .await
        {
            Ok(current) => wire_u64(current.participant.revision)?,
            Err(trellis_rs::client::CallError::Declared(error))
                if matches!(
                    error.as_ref(),
                    AuthParticipantsGetError::AuthError(error)
                        if error.payload().is_ok_and(|details| details.code.as_ref() == "not_found")
                ) =>
            {
                0
            }
            Err(error) => return Err(error).into_diagnostic(),
        },
    };
    let response = auth
        .participants_install(&auth_types::AuthParticipantsInstallRequest {
            expected_revision: wire(expected_revision.to_string())?,
            idempotency_key: wire(cli_idempotency_key())?,
            package_digest: wire(participant.package_digest)?,
            package_evidence: participant.package_evidence,
            participant_path: wire(participant.participant_path)?,
            platform_trust: Some(args.platform_trust),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else {
        output::print_success("installed participant definition");
        output::print_info(&format!(
            "participantId={}",
            response.participant.participant_id
        ));
        output::print_info(&format!(
            "revision={}",
            wire::<String>(response.participant.revision)?
        ));
    }
    Ok(())
}

async fn issuers_revoke_command(
    format: OutputFormat,
    args: &IssuersRevokeArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let response = AuthClient::from_generated(connected.clone())
        .issuers_revoke(&auth_types::AuthIssuersRevokeRequest {
            idempotency_key: wire(cli_idempotency_key())?,
            key_id: wire(&args.key_id)?,
            reason: wire(&args.reason)?,
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

#[cfg(test)]
mod tests {
    use super::{
        authenticated_user_json, pending_agent_login_json, render_agent_login_instructions,
    };
    use serde_json::json;
    use trellis_rs::auth::AuthenticatedUser;

    #[test]
    fn agent_login_instructions_include_plain_url_and_terminal_qr() {
        let instructions =
            render_agent_login_instructions("https://auth.example.com/login?flowId=flow_123")
                .expect("render instructions");

        assert!(instructions.contains("Open this activation URL:"));
        assert!(instructions.contains("https://auth.example.com/login?flowId=flow_123"));
        assert!(instructions.contains("Scan this QR code:"));
        assert!(
            instructions.contains("█") || instructions.contains("▀") || instructions.contains("▄")
        );
    }

    #[test]
    fn pending_agent_login_json_includes_login_url() {
        assert_eq!(
            pending_agent_login_json("https://auth.example.com/login?flowId=flow_123"),
            json!({
                "status": "pending",
                "loginUrl": "https://auth.example.com/login?flowId=flow_123",
            })
        );
    }

    #[test]
    fn authenticated_user_output_is_account_first() {
        let user = AuthenticatedUser {
            principal_id: "usr_123".to_string(),
            state: "active".to_string(),
            email: Some("ada@example.com".to_string()),
            image: None,
            name: Some("Ada".to_string()),
            user_id: "usr_123".to_string(),
        };

        assert_eq!(
            authenticated_user_json(&user),
            json!({
                "userId": "usr_123",
                "principalId": "usr_123",
                "state": "active",
                "name": "Ada",
            })
        );
    }
}
