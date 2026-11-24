use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};
use trellis_idl::project::read_manifest;
use trellis_idl::CanonicalMode;
use trellis_rs::auth as authlib;
use trellis_rs::generated::Client;
use trellis_runtime_apis::types as auth_types;

use crate::app::{connect_authenticated_cli_client, generate_session_keypair, wire};
use crate::cli::*;
use crate::output;

pub(super) async fn run_svc(format: OutputFormat, command: SvcCommand) -> miette::Result<()> {
    match (command.id, command.command) {
        (None, SvcSubcommand::List(args)) => list_services(format, &args).await,
        (None, SvcSubcommand::Import(args)) => super::bundle::import(format, &args).await,
        (Some(_), SvcSubcommand::Import(_)) => Err(miette::miette!(
            "`import` is a top-level service command; use `trellis svc import <FILE>`"
        )),
        (Some(id), SvcSubcommand::Resource(action)) => {
            run_svc_resource(format, SvcResourceCommand { id, action }).await
        }
        (Some(_), SvcSubcommand::List(_)) => Err(miette::miette!(
            "`list` is a top-level service command; use `trellis svc list`"
        )),
        (None, SvcSubcommand::Resource(_)) => Err(miette::miette!(
            "missing service deployment ID; use `trellis svc <ID> <COMMAND>`"
        )),
    }
}

pub(super) async fn run_dev(format: OutputFormat, command: DevCommand) -> miette::Result<()> {
    match (command.id, command.command) {
        (None, DevSubcommand::List(args)) => list_devices(format, &args).await,
        (Some(id), DevSubcommand::Resource(action)) => {
            run_dev_resource(format, DevResourceCommand { id, action }).await
        }
        (Some(_), DevSubcommand::List(_)) => Err(miette::miette!(
            "`list` is a top-level device command; use `trellis dev list`"
        )),
        (None, DevSubcommand::Resource(_)) => Err(miette::miette!(
            "missing device deployment ID; use `trellis dev <ID> <COMMAND>`"
        )),
    }
}

async fn run_svc_resource(format: OutputFormat, command: SvcResourceCommand) -> miette::Result<()> {
    match command.action {
        SvcResourceAction::Show => show_service(format, &command.id).await,
        SvcResourceAction::Create(args) => create_service(format, &command.id, &args).await,
        SvcResourceAction::Apply(args) => {
            apply_contract(format, DeploymentKind::Service, &command.id, &args).await
        }
        SvcResourceAction::Disable(args) => {
            toggle_deployment(
                format,
                &command.id,
                false,
                DeploymentKind::Service,
                args.force,
            )
            .await
        }
        SvcResourceAction::Enable => {
            toggle_deployment(format, &command.id, true, DeploymentKind::Service, false).await
        }
        SvcResourceAction::Remove(args) => {
            remove_deployment(format, DeploymentKind::Service, &command.id, &args).await
        }
        SvcResourceAction::Instances(args) => service_instances(format, &command.id, &args).await,
        SvcResourceAction::Provision(args) => provision_service(format, &command.id, &args).await,
        SvcResourceAction::Enrollment(enrollment) => {
            super::enrollment::run(format, &command.id, enrollment).await
        }
        SvcResourceAction::InstancePolicy(policy) => {
            super::enrollment::deployment_policy(format, &command.id, policy).await
        }
        SvcResourceAction::Updates => service_updates(format, &command.id).await,
        SvcResourceAction::Approve(args) => {
            decide_service_update(format, &command.id, &args, true).await
        }
        SvcResourceAction::Reject(args) => {
            decide_service_update(format, &command.id, &args, false).await
        }
        SvcResourceAction::Webhooks(args) => super::webhooks::run(format, &command.id, args).await,
        SvcResourceAction::Export(args) => super::bundle::export(format, &command.id, &args).await,
        SvcResourceAction::Config(args) => {
            super::config::run(
                format,
                super::config::Scope::Deployment(DeploymentKind::Service, &command.id),
                args,
            )
            .await
        }
    }
}

async fn list_service_updates(
    connected: &Client,
    deployment_id: &str,
) -> miette::Result<Vec<auth_types::AuthServiceUpdate>> {
    trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1()
        .service_updates_list_items(auth_types::AuthServiceUpdatesListRequest {
            deployment_id: Some(wire(deployment_id)?),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()
}

async fn service_updates(format: OutputFormat, id: &str) -> miette::Result<()> {
    let deployment_id = resolve_update_owner(id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let updates = list_service_updates(&connected, &deployment_id).await?;
    if output::is_json(format) {
        return output::print_json(&json!({ "updates": updates }));
    }
    if updates.is_empty() {
        output::note(format!("{id} has not submitted any updates."));
    }
    for (index, update) in updates.iter().enumerate() {
        if index > 0 {
            anstream::println!();
        }
        print_service_update(update);
    }
    if updates
        .iter()
        .any(|update| update.state == auth_types::AuthServiceUpdateState::Pending)
    {
        output::next(&format!("trellis svc {id} approve"));
    }
    Ok(())
}

/// Print what an update asks for and gives up, in the Console's words.
fn print_service_update(update: &auth_types::AuthServiceUpdate) {
    let changes = &update.changes;
    let resource =
        |item: &auth_types::AuthServiceUpdateResource| format!("{} {}", item.kind, item.name);
    let value = serde_json::to_value(update).unwrap_or_default();
    let mut details = output::Details::new(format!("Update {}", output::field(&value, "updateId")))
        .state("State", &output::field(&value, "state"))
        .field("Participant", output::field(&value, "participantId"))
        .time("Requested", &output::field(&value, "requestedAt"))
        .time("Decided", &output::field(&value, "decidedAt"));
    if update.state == auth_types::AuthServiceUpdateState::Pending {
        if let Some(review_url) = &update.review_url {
            details = details.field("Review", review_url);
        }
    }
    let section = |details: output::Details, title: &str, lines: Vec<String>| {
        details.section(title).items(lines)
    };
    let details = section(
        details,
        "Asks for",
        changes
            .added_capabilities
            .iter()
            .map(|capability| format!("capability {capability}"))
            .chain(changes.added_resources.iter().map(resource))
            .chain(
                changes
                    .reattached_resources
                    .iter()
                    .map(|item| format!("{} again, with its kept data", resource(item))),
            )
            .chain(changes.added_apis.iter().map(|api| {
                if changes.shared_apis.contains(api) {
                    format!("to provide {api}, which another service already serves")
                } else {
                    format!("to provide {api}")
                }
            }))
            .chain(
                changes
                    .changed_resources
                    .iter()
                    .map(|item| format!("{} (changed)", resource(item))),
            )
            .chain(
                changes
                    .accepted_used_apis
                    .iter()
                    .map(|api| format!("to add {api}, which it uses, to the catalog")),
            )
            .collect(),
    );
    let details = section(
        details,
        "Gives up",
        changes
            .removed_capabilities
            .iter()
            .map(|capability| format!("capability {capability}"))
            .chain(
                changes
                    .removed_resources
                    .iter()
                    .map(|item| format!("{} (data kept)", resource(item))),
            )
            .collect(),
    );
    let details = section(
        details,
        "Provides",
        changes
            .removed_apis
            .iter()
            .map(|api| format!("stops {api}"))
            .chain(
                changes
                    .accepted_apis
                    .iter()
                    .map(|api| format!("updates {api}")),
            )
            .collect(),
    );
    details.print();
}

async fn decide_service_update(
    format: OutputFormat,
    id: &str,
    args: &SvcUpdateArgs,
    approve: bool,
) -> miette::Result<()> {
    let deployment_id = resolve_update_owner(id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let updates = list_service_updates(&connected, &deployment_id).await?;
    let selected = match &args.update {
        Some(update) => updates
            .iter()
            .find(|candidate| candidate.update_id.to_string() == *update),
        None => updates
            .iter()
            .find(|candidate| candidate.state == auth_types::AuthServiceUpdateState::Pending),
    }
    .ok_or_else(|| match &args.update {
        Some(update) => miette::miette!("{id} has no update {update}"),
        None => miette::miette!("{id} has no pending update"),
    })?;
    let unauthenticated = selected
        .changes
        .webhooks
        .iter()
        .filter(|webhook| webhook.unauthenticated)
        .map(|webhook| webhook.name.clone())
        .collect::<Vec<_>>();
    if approve && !unauthenticated.is_empty() && !args.acknowledge_unauthenticated_webhooks {
        return Err(miette::miette!(
            help = "Re-run with --acknowledge-unauthenticated-webhooks only if that is intended.",
            "This update adds webhook endpoint {} that checks nothing: anyone who learns its \
            URL can send {id} requests.",
            unauthenticated.join(", ")
        ));
    }
    if approve && !selected.changes.shared_apis.is_empty() && !args.acknowledge_shared_apis {
        return Err(miette::miette!(
            help = "Re-run with --acknowledge-shared-apis only if splitting requests is intended.",
            "This update makes {id} answer {} alongside the service that already does. \
            Callers' requests would be split between the two services at random: each sees \
            only some requests, they keep separate data, and callers cannot tell which one \
            answered. This is not how replicas or upgrades work; add instances to one \
            service for that.",
            selected
                .changes
                .shared_apis
                .iter()
                .map(|api| api.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .service_updates_decide(&auth_types::AuthServiceUpdateDecideRequest {
            update_id: selected.update_id,
            acknowledge_shared_apis: (approve && args.acknowledge_shared_apis).then_some(true),
            acknowledge_unauthenticated_webhooks: (approve
                && args.acknowledge_unauthenticated_webhooks)
                .then_some(true),
            decision: if approve {
                auth_types::AuthDecision::Approved
            } else {
                auth_types::AuthDecision::Declined
            },
            expected_revision: selected.revision,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        return output::print_json(&json!({ "update": response.update }));
    }
    let name = output::strong(id);
    match response.update.state {
        auth_types::AuthServiceUpdateState::Applied => output::success(format!(
            "Approved the update; {name} connects with its new build now"
        )),
        auth_types::AuthServiceUpdateState::Rejected => output::success(format!(
            "Rejected the update; {name} keeps its installed build"
        )),
        _ => output::warning(format!(
            "{name} changed since it submitted this update; it submits again when it reconnects"
        )),
    }
    Ok(())
}

async fn run_dev_resource(format: OutputFormat, command: DevResourceCommand) -> miette::Result<()> {
    let id = command.id;
    match command.action {
        DevResourceAction::Show => show_device(format, &id).await,
        DevResourceAction::Create(args) => create_device(format, &id, &args).await,
        DevResourceAction::Apply(args) => {
            apply_contract(format, DeploymentKind::Device, &id, &args).await
        }
        DevResourceAction::Disable => {
            toggle_deployment(format, &id, false, DeploymentKind::Device, false).await
        }
        DevResourceAction::Enable => {
            toggle_deployment(format, &id, true, DeploymentKind::Device, false).await
        }
        DevResourceAction::Remove(args) => {
            remove_deployment(format, DeploymentKind::Device, &id, &args).await
        }
        DevResourceAction::Instances(args) => device_instances(format, &id, &args).await,
        DevResourceAction::Provision(args) => provision_device(format, &id, &args).await,
        DevResourceAction::Config(args) => {
            super::config::run(
                format,
                super::config::Scope::Deployment(DeploymentKind::Device, &id),
                args,
            )
            .await
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum DeploymentKind {
    Service,
    Device,
}

/// A compiled participant plus the package evidence `Auth.Deployments.Apply` requires.
///
/// Public so Rust tooling (for example the test harness) can apply a participant from a source
/// directory through the same compilation path the CLI uses.
pub struct CompiledParticipantInput {
    /// Participant identity, for example `tsd-survey-service.Survey`.
    pub participant_id: String,
    /// Semantic participant digest.
    pub participant_digest: String,
    /// Participant path inside the package.
    pub participant_path: String,
    /// Semantic digest of the root package.
    pub package_digest: String,
    /// Full exact package closure evidence.
    pub package_evidence: auth_types::AuthPackageEvidence,
    /// APIs the participant implements, which must be accepted to deploy it.
    pub implemented_apis: Vec<String>,
}

/// The exact package closure evidence Auth compiles a build from.
pub(super) fn package_evidence(
    compiled: &trellis_idl::PackageGraph,
) -> miette::Result<auth_types::AuthPackageEvidence> {
    Ok(auth_types::AuthPackageEvidence {
        root_package: auth_types::AuthName(compiled.root().as_str().to_owned()),
        root_digest: auth_types::AuthDigest(compiled.root_digest().to_owned()),
        packages: compiled
            .packages()
            .iter()
            .map(|(id, package)| {
                Ok(auth_types::AuthPackageSourceEvidence {
                    name: auth_types::AuthName(id.as_str().to_owned()),
                    version: auth_types::AuthShortName(package.version().to_string()),
                    digest: auth_types::AuthDigest(
                        compiled
                            .digest(id)
                            .expect("compiled package digest")
                            .to_owned(),
                    ),
                    source: trellis_idl::canonical_package(
                        compiled,
                        id,
                        CanonicalMode::Presentation,
                    )?,
                    extra: Default::default(),
                })
            })
            .collect::<miette::Result<_>>()?,
        extra: Default::default(),
    })
}

/// Compile a source project and select its unique participant of the requested kind.
/// Returns the exact package closure used by deployment creation and application.
pub fn compile_participant_input(
    source: &std::path::Path,
    selected_participant: Option<&str>,
    expected_kind: Option<trellis_idl::ParticipantKind>,
) -> miette::Result<CompiledParticipantInput> {
    let root = source.canonicalize().into_diagnostic()?;
    let manifest = read_manifest(&root.join("trellis.toml"))?;
    let compiled = crate::package::compile_project(&root, &manifest)?;
    let candidates = compiled
        .root_package()
        .participants()
        .values()
        .filter(|participant| expected_kind.is_none_or(|kind| participant.kind() == kind))
        .collect::<Vec<_>>();
    let participant = candidates[choose_participant(
        &candidates
            .iter()
            .map(|candidate| candidate.identity().as_str().to_owned())
            .collect::<Vec<_>>(),
        selected_participant,
    )?];
    let participant_path = participant.name().to_owned();
    let participant_id = participant.identity().as_str().to_owned();
    let participant_digest = trellis_idl::participant_digest(&compiled, participant.identity())?;
    let implemented_apis = participant
        .implements()
        .iter()
        .map(ToString::to_string)
        .collect();
    let package_evidence = package_evidence(&compiled)?;
    Ok(CompiledParticipantInput {
        participant_id,
        participant_digest,
        participant_path,
        package_digest: compiled.root_digest().to_owned(),
        package_evidence,
        implemented_apis,
    })
}

/// The participant `selected` names among `candidates`, the only one, or the
/// one chosen at a terminal.
fn choose_participant(candidates: &[String], selected: Option<&str>) -> miette::Result<usize> {
    match selected {
        // A full ID names itself; a short ID, without the workspace, names
        // the one candidate that has it.
        Some(id) => {
            let matching = candidates
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    *candidate == id || trellis_protocol::short_id(candidate) == id
                })
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [(index, _)] => Ok(*index),
                [] => Err(miette::miette!(
                    "participant '{id}' does not match; candidates: {}",
                    candidates.join(", ")
                )),
                several => Err(miette::miette!(
                    "participant '{id}' could mean {}; give its full ID",
                    several
                        .iter()
                        .map(|(_, candidate)| candidate.as_str())
                        .collect::<Vec<_>>()
                        .join(" or ")
                )),
            }
        }
        None if candidates.len() == 1 => Ok(0),
        None if crate::prompt::interactive() && !candidates.is_empty() => {
            let choices = candidates.iter().cloned().enumerate().collect::<Vec<_>>();
            crate::prompt::Form::new(true).choose(
                "Which participant?",
                "--participant",
                None,
                &choices,
                None,
            )
        }
        None if candidates.is_empty() => Err(miette::miette!(
            "the package declares no participant of this kind"
        )),
        None => Err(miette::miette!(
            "select one participant with --participant; candidates: {}",
            candidates.join(", ")
        )),
    }
}

/// The participant and package evidence a create or apply installs: compiled
/// here from `--source`, or pulled from a registry and checked by Auth from
/// `--package`.
async fn build_input(
    format: OutputFormat,
    connected: &Client,
    build: &BuildArgs,
    kind: trellis_idl::ParticipantKind,
) -> miette::Result<(String, auth_types::AuthPackageEvidence)> {
    if let Some(source) = &build.source {
        let participant =
            compile_participant_input(source, build.participant.as_deref(), Some(kind))?;
        return Ok((participant.participant_id, participant.package_evidence));
    }
    let package = build
        .package
        .as_deref()
        .ok_or_else(|| miette::miette!("give --source DIR or --package WORKSPACE/NAME@VERSION"))?;
    let (name, release) = package.rsplit_once('@').ok_or_else(|| {
        miette::miette!(
            help = "Name a release as ghcr.io/acme/orders@1.2.0, or pin it as \
            ghcr.io/acme/orders@sha256:<digest>.",
            "--package {package} names no release"
        )
    })?;
    miette::WrapErr::wrap_err_with(trellis_idl::project::package_repository(name), || {
        format!("--package {package} must name a published package by its full identity, such as ghcr.io/acme/orders@1.2.0")
    })?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
        .trellis_auth_v1();
    let pinned = release.starts_with("sha256:");
    let pulled = auth
        .packages_pull(&auth_types::AuthPackagePullRequest {
            package: wire(name)?,
            version: (!pinned).then(|| wire(release)).transpose()?,
            digest: pinned.then(|| wire(release)).transpose()?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    let wanted = match kind {
        trellis_idl::ParticipantKind::Service => auth_types::AuthDeploymentKind::Service,
        trellis_idl::ParticipantKind::Device => auth_types::AuthDeploymentKind::Device,
    };
    let candidates = pulled
        .participants
        .iter()
        .filter(|participant| participant.kind == wanted)
        .map(|participant| participant.participant_id.clone())
        .collect::<Vec<_>>();
    let participant =
        candidates[choose_participant(&candidates, build.participant.as_deref())?].clone();
    if !output::is_json(format) {
        let published = &pulled.published;
        output::Details::new(format!(
            "{} {}",
            output::strong(&published.package.0),
            published.version.0
        ))
        .field("Digest", &published.digest.0)
        .field("Participant", &participant)
        .section("Built with")
        .items(
            pulled
                .dependencies
                .iter()
                .map(|dependency| format!("{} {}", dependency.name.0, dependency.version.0)),
        )
        .print();
    }
    Ok((participant, pulled.package_evidence))
}

/// Run one `trellis registries` command.
pub(super) async fn registries(
    format: OutputFormat,
    command: RegistriesCommand,
) -> miette::Result<()> {
    let (_, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    match command {
        RegistriesCommand::List => {
            let listed = auth
                .package_registries_list(&auth_types::AuthEmpty {
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&listed);
            }
            output::table(
                &listed.registries,
                &[
                    output::col("Name", "name"),
                    output::col("Prefix", "prefix"),
                    output::col("Signs in as", "username"),
                ],
                "No registries. Add one with `trellis registries set NAME --prefix HOST/PATH`.",
            )
        }
        RegistriesCommand::Set(args) => {
            let credentials = match &args.username {
                None => None,
                Some(username) => {
                    let secret = match &args.secret_file {
                        Some(path) if path != std::path::Path::new("-") => {
                            std::fs::read_to_string(path).into_diagnostic()?
                        }
                        None if std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                            let mut form = crate::prompt::Form::new(true);
                            let typed = form.required_secret(
                                &format!("Password or token for {username}"),
                                "--secret-file PATH",
                            )?;
                            form.finish()?;
                            typed
                        }
                        _ => std::io::read_to_string(std::io::stdin()).into_diagnostic()?,
                    };
                    let secret = secret.trim_end_matches(['\r', '\n']);
                    miette::ensure!(!secret.is_empty(), "the registry secret is empty");
                    Some(auth_types::AuthPackageRegistryCredentials {
                        username: wire(username)?,
                        secret: wire(secret)?,
                        extra: Default::default(),
                    })
                }
            };
            let response = auth
                .package_registries_put(&auth_types::AuthPackageRegistryPutRequest {
                    name: wire(&args.name)?,
                    prefix: wire(&args.prefix)?,
                    credentials,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&response);
            }
            output::success(format!(
                "Saved registry {} at {}",
                output::strong(&response.registry.name.0),
                response.registry.prefix.0
            ));
            Ok(())
        }
        RegistriesCommand::Remove { name } => {
            auth.package_registries_remove(&auth_types::AuthPackageRegistryRemoveRequest {
                name: wire(&name)?,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&json!({ "removed": name }));
            }
            output::success(format!("Removed registry {}", output::strong(&name)));
            Ok(())
        }
    }
}

async fn list_services(format: OutputFormat, args: &SvcListArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployments =
        trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
            .trellis_auth_v1()
            .deployments_list_items(auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor: None,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .try_filter(|item| {
                std::future::ready(
                    item.kind == auth_types::AuthDeploymentKind::Service
                        && (args.disabled
                            || item.state == auth_types::AuthDeploymentState::Enabled),
                )
            })
            .try_collect::<Vec<_>>()
            .await
            .into_diagnostic()?;
    print_deployments(format, DeploymentKind::Service, &deployments, args.disabled)
}

async fn list_devices(format: OutputFormat, args: &DevListArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployments =
        trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
            .trellis_auth_v1()
            .deployments_list_items(auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor: None,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .try_filter(|item| {
                std::future::ready(
                    item.kind == auth_types::AuthDeploymentKind::Device
                        && (args.disabled
                            || item.state == auth_types::AuthDeploymentState::Enabled),
                )
            })
            .try_collect::<Vec<_>>()
            .await
            .into_diagnostic()?;
    print_deployments(format, DeploymentKind::Device, &deployments, args.disabled)
}

async fn show_service(format: OutputFormat, id: &str) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployment = find_deployment(&connected, id, DeploymentKind::Service).await?;
    print_deployment_show_result(format, DeploymentKind::Service, &deployment)
}

async fn show_device(format: OutputFormat, id: &str) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployment = find_deployment(&connected, id, DeploymentKind::Device).await?;
    print_deployment_show_result(format, DeploymentKind::Device, &deployment)
}

async fn create_service(
    format: OutputFormat,
    id: &str,
    args: &SvcCreateArgs,
) -> miette::Result<()> {
    if args.build.source.is_none() && args.build.package.is_none() {
        return create_service_claim(format, id, args.public_key.as_deref()).await;
    }
    let (_, connected) = connect_authenticated_cli_client().await?;
    let (participant_id, package_evidence) = build_input(
        format,
        &connected,
        &args.build,
        trellis_idl::ParticipantKind::Service,
    )
    .await?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .deployments_create(&auth_types::AuthDeploymentCreateRequest {
            kind: auth_types::AuthDeploymentKind::Service,
            title: wire(id)?,
            participant_id,
            package_evidence,
            acknowledge_shared_apis: args.acknowledge_shared_apis.then_some(true),
            acknowledge_unauthenticated_webhooks: args
                .acknowledge_unauthenticated_webhooks
                .then_some(true),
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .map_err(shared_api_hint)?;
    print_deployment_result(
        format,
        "Created",
        DeploymentKind::Service,
        &response.deployment,
    )?;
    output::next(&format!("trellis svc {id} provision"));
    Ok(())
}

/// Create a claim the first service to connect with its identity fills in, for
/// a service whose IDL the administrator does not have beforehand. The identity
/// is the owner's public key, or one generated here whose seed is printed once.
async fn create_service_claim(
    format: OutputFormat,
    id: &str,
    public_key: Option<&str>,
) -> miette::Result<()> {
    let (seed, key) = match public_key {
        Some(key) => (None, parse_public_key(key)?),
        None => {
            let (seed, key) = generate_session_keypair();
            (Some(seed), key)
        }
    };
    let (_, connected) = connect_authenticated_cli_client().await?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .service_claims_create(&auth_types::AuthServiceClaimCreateRequest {
            title: wire(id)?,
            instance_title: wire(id)?,
            identity_public_key: wire(&key)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        return output::print_json(&json!({ "claim": response.claim, "instanceSeed": seed }));
    }
    output::success(format!(
        "Created a claim for service {}",
        output::strong(id)
    ));
    output::Details::untitled()
        .field("Claim", response.claim.claim_id)
        .field("Identity key", &key)
        .field("Instance seed", seed.as_deref().unwrap_or_default())
        .print();
    if seed.is_some() {
        output::not_kept(
            "instance seed",
            "Give it to the service; when it connects it asks for what it needs.",
        );
    } else {
        output::hint("When the service connects with this key's seed, it asks for what it needs.");
    }
    output::next(&format!("trellis svc {id} updates"));
    Ok(())
}

/// Check that `key` is an Ed25519 public key in base64url, so a typo is refused
/// before it names an identity no service can prove.
fn parse_public_key(key: &str) -> miette::Result<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(key)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
    miette::ensure!(
        bytes.is_some_and(|bytes| ed25519_dalek::VerifyingKey::from_bytes(&bytes).is_ok()),
        help = "Pass the public key `trellis keys new` printed.",
        "--public-key is not a base64url Ed25519 public key"
    );
    Ok(key.to_owned())
}

/// Auth refuses to let a deployment answer an API another one serves unless the
/// caller confirms it; say how to confirm from the CLI.
fn shared_api_hint(error: impl std::fmt::Display) -> miette::Report {
    let message = error.to_string();
    if message.contains("acknowledgeSharedApis") {
        miette::miette!(
            help = "Re-run with --acknowledge-shared-apis only if splitting requests is intended.",
            "{message}"
        )
    } else if message.contains("acknowledgeUnauthenticatedWebhooks") {
        miette::miette!(
            help = "Re-run with --acknowledge-unauthenticated-webhooks only if that is intended.",
            "{message}"
        )
    } else {
        miette::miette!("{message}")
    }
}

async fn create_device(format: OutputFormat, id: &str, args: &DevCreateArgs) -> miette::Result<()> {
    let (_, connected) = connect_authenticated_cli_client().await?;
    let (participant_id, package_evidence) = build_input(
        format,
        &connected,
        &args.build,
        trellis_idl::ParticipantKind::Device,
    )
    .await?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .deployments_create(&auth_types::AuthDeploymentCreateRequest {
            kind: auth_types::AuthDeploymentKind::Device,
            title: wire(id)?,
            participant_id,
            package_evidence,
            acknowledge_shared_apis: args.acknowledge_shared_apis.then_some(true),
            acknowledge_unauthenticated_webhooks: args
                .acknowledge_unauthenticated_webhooks
                .then_some(true),
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .map_err(shared_api_hint)?;
    print_deployment_result(
        format,
        "Created",
        DeploymentKind::Device,
        &response.deployment,
    )?;
    output::next(&format!("trellis dev {id} provision"));
    Ok(())
}

async fn apply_contract(
    format: OutputFormat,
    kind: DeploymentKind,
    deployment_id: &str,
    args: &ApplyArgs,
) -> miette::Result<()> {
    let expected_kind = match kind {
        DeploymentKind::Service => trellis_idl::ParticipantKind::Service,
        DeploymentKind::Device => trellis_idl::ParticipantKind::Device,
    };
    let (_, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, deployment_id, kind).await?;
    // The deployment's own participant is the one to install when none is
    // named, even in a package with several.
    let build = BuildArgs {
        participant: args
            .build
            .participant
            .clone()
            .or_else(|| current["participantId"].as_str().map(str::to_owned)),
        ..args.build.clone()
    };
    let (participant_id, package_evidence) =
        build_input(format, &connected, &build, expected_kind).await?;
    miette::ensure!(
        current["participantId"].as_str() == Some(&participant_id),
        "deployment participant does not match the selected source participant"
    );
    let deployment_id = current["deploymentId"]
        .as_str()
        .ok_or_else(|| miette::miette!("deployment response missing deploymentId"))?;
    let revision = current["revision"]
        .as_str()
        .ok_or_else(|| miette::miette!("deployment response missing revision"))?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .deployments_apply(&auth_types::AuthDeploymentApplyRequest {
            deployment_id: wire(deployment_id)?,
            expected_revision: wire(
                args.expected_revision
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| revision.to_owned()),
            )?,
            idempotency_key: wire(cli_idempotency_key())?,
            package_evidence,
            acknowledge_shared_apis: args.acknowledge_shared_apis.then_some(true),
            acknowledge_unauthenticated_webhooks: args
                .acknowledge_unauthenticated_webhooks
                .then_some(true),
            extra: Default::default(),
        })
        .await
        .map_err(shared_api_hint)?;
    print_deployment_result(format, "Applied the package to", kind, &response.deployment)
}

/// Ask before `what` (`remove`, `disable`) changes `current`, naming the
/// installation it acts on, unless `force`. Disabling asks only for a
/// deployment imported here within the last day.
///
/// A moved deployment keeps its ID, so one ID names it in the installation it
/// left and in the one it moved to, and a command run against the wrong one
/// takes down the service that was just moved. A fresh import is when the
/// installation it moved from gets cleaned up, so that is when it is called
/// out; a day covers the move guide's steps without asking forever.
fn confirm_change(
    installation: &str,
    current: &Value,
    kind: DeploymentKind,
    id: &str,
    what: &str,
    force: bool,
) -> miette::Result<()> {
    let imported = current["importedAt"].as_str().filter(|at| {
        time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
            .is_ok_and(|at| time::OffsetDateTime::now_utc() - at < time::Duration::DAY)
    });
    if force || (what == "disable" && imported.is_none()) {
        return Ok(());
    }
    let title = output::field(current, "title");
    let deployment_id = output::field(current, "deploymentId");
    if let Some(at) = imported {
        output::warning(format!(
            "{title} ({deployment_id}) was imported into {installation} at {}. The \
            installation it moved from has a deployment with the same ID; check that \
            {installation} is the one you mean.",
            output::absolute_time(at)
        ));
    }
    output::confirm_typed(
        &format!(
            "{} {title} ({deployment_id}) on {installation}",
            kind_noun(kind)
        ),
        id,
        what,
        "-f",
    )
}

async fn toggle_deployment(
    format: OutputFormat,
    id: &str,
    enable: bool,
    kind: DeploymentKind,
    force: bool,
) -> miette::Result<()> {
    let (selection, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, id, kind).await?;
    if !enable {
        confirm_change(
            &selection.scope.issuer,
            &current,
            kind,
            id,
            "disable",
            force,
        )?;
    }
    let request = auth_types::AuthDeploymentMutationRequest {
        deployment_id: wire(&current["deploymentId"])?,
        expected_revision: wire(&current["revision"])?,
        idempotency_key: wire(cli_idempotency_key())?,
        extra: Default::default(),
    };
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let response = if enable {
        auth.deployments_enable(&request).await.into_diagnostic()?
    } else {
        auth.deployments_disable(&request).await.into_diagnostic()?
    };
    print_deployment_result(
        format,
        if enable { "Enabled" } else { "Disabled" },
        kind,
        &response.deployment,
    )
}

async fn remove_deployment(
    format: OutputFormat,
    kind: DeploymentKind,
    id: &str,
    args: &RemoveArgs,
) -> miette::Result<()> {
    miette::ensure!(
        !output::is_json(format) || args.force,
        help = "Pass -f to remove it without being asked.",
        "refusing to remove {} without confirmation in JSON mode",
        ref_label(kind, id)
    );
    let (selection, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, id, kind).await?;
    // The prompt and the report name the deployment the selector resolved
    // to, so a stale selector never removes a deployment the operator did not see.
    let deployment_id = output::field(&current, "deploymentId");
    let title = output::field(&current, "title");
    confirm_change(
        &selection.scope.issuer,
        &current,
        kind,
        id,
        "remove",
        args.force,
    )?;
    let response =
        trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
            .trellis_auth_v1()
            .deployments_remove(&auth_types::AuthDeploymentMutationRequest {
                deployment_id: wire(&current["deploymentId"])?,
                expected_revision: wire(&current["revision"])?,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            })
            .await
            .into_diagnostic()?;
    output::emit(
        format,
        &json!({
            "success": serde_json::to_value(response).is_ok(),
            "deploymentId": deployment_id,
            "title": title,
        }),
        |_| {
            output::success(format!(
                "Removed {} {} ({deployment_id})",
                kind_noun(kind),
                output::strong(&title)
            ));
            Ok(())
        },
    )
}

async fn service_instances(
    format: OutputFormat,
    id: &str,
    args: &SvcInstancesArgs,
) -> miette::Result<()> {
    let (_, connected) = connect_authenticated_cli_client().await?;
    let deployment = find_deployment(&connected, id, DeploymentKind::Service).await?;
    let deployment_id = output::field(&deployment, "deploymentId");
    let client = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected);
    let instances = client
        .trellis_auth_v1()
        .service_instances_list_items(auth_types::AuthInstancesListRequest {
            deployment_id: Some(wire(&deployment_id)?),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .try_filter(|item| {
            let ended = matches!(
                item.state,
                auth_types::AuthInstanceState::Retired | auth_types::AuthInstanceState::Removed
            );
            std::future::ready(if ended {
                args.all
            } else {
                args.disabled || args.all || item.principal.enabled
            })
        })
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()?;
    // Health is what each instance last reported; without it the listing
    // still shows what Auth knows.
    let health = client
        .trellis_health_v1()
        .inspect(&auth_types::HealthInspectRequest {
            contract_id: auth_types::HealthInspectRequestContractId(output::field(
                &deployment,
                "participantId",
            )),
            history_limit: Some(auth_types::HealthInspectRequestHistoryLimit(1.into())),
            history_since: None,
            instance_id: None,
            participant_kind: auth_types::HealthInspectRequestParticipantKind::Service,
            extra: Default::default(),
        })
        .await
        .ok()
        .and_then(|health| serde_json::to_value(health).ok());
    let instances = instances
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    let instances = with_status(
        instances,
        health
            .as_ref()
            .map_or(&[][..], |health| output::list(health, "instances")),
        &output::field(&deployment, "implementationDigest"),
    );
    print_service_instances_result(format, instances)
}

/// Joins what Auth knows of each instance with what it last reported, so a
/// person or a script can follow an upgrade: which build each one runs,
/// whether that is the deployment's current build, and its state.
///
/// An instance that is not connected is `offline`. A connected one is in the
/// phase its latest heartbeat reports, when that heartbeat is current and
/// came on this connection; before its first heartbeat it is
/// `waitingForApproval` while its build's update waits for an administrator,
/// `waitingForSettings` while required settings have no value, and
/// `starting` otherwise. A heartbeat still reporting `connecting` also reads
/// as `starting`: the instance is connected but not serving yet.
///
/// Its health is the report's effective status and the checks that heartbeat
/// carries as failing; an instance that has not reported on this connection
/// has none. Once a connected instance's heartbeats stop, it keeps the phase
/// it last reported and its health is `offline` with no failing checks.
fn with_status(instances: Vec<Value>, health: &[Value], current: &str) -> Vec<Value> {
    instances
        .into_iter()
        .map(|mut instance| {
            let id = output::field(&instance, "instanceId");
            let report = health
                .iter()
                .find(|report| output::field(report, "instanceId") == id);
            let connected_at = instance.get("connectedAt").and_then(Value::as_str);
            let heartbeat = report.and_then(|report| report.pointer("/latestSample/sample/time"));
            // A report from an earlier connection says nothing about this one.
            let reported = report.filter(|report| {
                match (
                    connected_at,
                    report.get("observedAt").and_then(Value::as_str),
                ) {
                    (Some(connected), Some(observed)) => {
                        output::parse_time(observed) >= output::parse_time(connected)
                    }
                    _ => false,
                }
            });
            let reporting =
                reported.filter(|report| output::field(report, "effectiveStatus") != "offline");
            let waiting = instance.get("waiting");
            let ended = output::field(&instance, "state");
            let state = if ended == "retired" || ended == "removed" {
                ended
            } else if connected_at.is_none() {
                "offline".to_owned()
            } else if let Some(report) = reported {
                report
                    .pointer("/latestSample/phase")
                    .and_then(Value::as_str)
                    .map(|phase| {
                        if phase == "connecting" {
                            "starting"
                        } else {
                            phase
                        }
                    })
                    .unwrap_or("serving")
                    .to_owned()
            } else if waiting.and_then(|waiting| waiting.get("update")).is_some() {
                "waitingForApproval".to_owned()
            } else if waiting
                .and_then(|waiting| waiting.get("missingSettings"))
                .and_then(Value::as_array)
                .is_some_and(|missing| !missing.is_empty())
            {
                "waitingForSettings".to_owned()
            } else {
                "starting".to_owned()
            };
            let digest = reported
                .and_then(|report| report.get("contractDigest"))
                .or_else(|| instance.get("participantDigest"))
                .and_then(Value::as_str);
            let version =
                reported.and_then(|report| report.pointer("/latestSample/participant/version"));
            // Health is the instance's effective status with the checks its
            // current heartbeat reports failing. A connected instance whose
            // heartbeats stopped is offline, without the checks it last
            // reported; a disconnected instance is offline whatever it last
            // reported.
            let health = if connected_at.is_none() {
                json!({ "status": "offline", "failingChecks": [] })
            } else if let Some(report) = reporting {
                let failing = report
                    .pointer("/latestSample/checks")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .filter(|check| output::field(check, "status") != "ok")
                    .map(|check| {
                        let mut failing = json!({
                            "name": check.get("name"),
                            "status": check.get("status"),
                            "summary": check.get("summary"),
                            "error": check.get("error"),
                        });
                        if let Some(failing) = failing.as_object_mut() {
                            failing.retain(|_, value| !value.is_null());
                        }
                        failing
                    })
                    .collect::<Vec<_>>();
                json!({ "status": report.get("effectiveStatus"), "failingChecks": failing })
            } else if reported.is_some() {
                json!({ "status": "offline", "failingChecks": [] })
            } else {
                Value::Null
            };
            let mut status = json!({
                "state": state,
                "health": health,
                "build": {
                    "digest": digest,
                    "version": version,
                    "current": digest.is_some_and(|digest| digest == current),
                },
                "connectedAt": connected_at,
                "lastHeartbeat": heartbeat,
                "reviewUrl": waiting.and_then(|waiting| waiting.pointer("/update/reviewUrl")),
                "missingSettings": waiting.and_then(|waiting| waiting.get("missingSettings")),
            });
            if let Some(status) = status.as_object_mut() {
                status.retain(|_, value| !value.is_null());
            }
            if let Some(build) = status.get_mut("build").and_then(Value::as_object_mut) {
                build.retain(|_, value| !value.is_null());
            }
            // The table shows the build as one short word.
            let label = match (version.and_then(Value::as_str), digest) {
                (Some(version), _) => version.to_owned(),
                (None, Some(digest)) => digest.chars().take(12).collect(),
                (None, None) => String::new(),
            };
            let older = !label.is_empty() && status["build"]["current"] != json!(true);
            status["build"]["label"] = json!(if older {
                format!("{label} (older)")
            } else {
                label
            });
            instance["status"] = status;
            instance
        })
        .collect()
}

async fn device_instances(
    format: OutputFormat,
    id: &str,
    args: &DevInstancesArgs,
) -> miette::Result<()> {
    let deployment_id = resolve_deployment_id(DeploymentKind::Device, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let instances = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .devices_list_items(auth_types::AuthInstancesListRequest {
            deployment_id: Some(wire(deployment_id)?),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .try_filter(|item| std::future::ready(args.disabled || item.principal.enabled))
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()?;
    print_device_instances_result(format, instances)
}

async fn provision_service(
    format: OutputFormat,
    id: &str,
    args: &SvcProvisionArgs,
) -> miette::Result<()> {
    let supplied_key = args
        .public_key
        .as_deref()
        .map(parse_public_key)
        .transpose()?;
    let deployment_id = resolve_deployment_id(DeploymentKind::Service, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let client = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let current = client
        .service_instances_list_items(auth_types::AuthInstancesListRequest {
            deployment_id: Some(wire(&deployment_id)?),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .try_filter(|item| std::future::ready(item.principal.enabled))
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()?;
    let existing = current.iter().find(|item| {
        supplied_key.as_ref().is_some_and(|key| {
            item.identity_public_key
                .as_ref()
                .is_some_and(|public| public.as_ref() == key)
        })
    });
    if !args.new {
        // An owner's key selects its instance or registers a new one; without
        // a key, re-running provision selects the only instance.
        match (&supplied_key, existing, current.as_slice()) {
            (Some(_), Some(existing), _) | (None, _, [existing]) => {
                return print_service_provision_result(format, existing, None, true)
            }
            (Some(_), None, _) | (None, _, []) => {}
            (None, _, _) => {
                return Err(miette::miette!(
                    "the service has several instances; use --new for another, or \
                    --public-key to select one"
                ))
            }
        }
    }
    miette::ensure!(
        existing.is_none(),
        "identity already belongs to an existing instance"
    );
    let (seed, key) = match supplied_key {
        Some(key) => (None, key),
        None => {
            let (seed, key) = generate_session_keypair();
            (Some(seed), key)
        }
    };
    let response = client
        .service_instances_provision(&auth_types::AuthInstanceProvisionRequest {
            deployment_id: wire(deployment_id)?,
            identity_public_key: Some(wire(key)?),
            title: wire(id)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    print_service_provision_result(format, &response.instance, seed.as_deref(), false)
}

async fn provision_device(
    format: OutputFormat,
    id: &str,
    args: &DevProvisionArgs,
) -> miette::Result<()> {
    let deployment_id = resolve_deployment_id(DeploymentKind::Device, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let seed: [u8; 32] = rand::random();
    let root_secret = URL_SAFE_NO_PAD.encode(seed);
    let identity = authlib::derive_device_identity(&seed).into_diagnostic()?;
    let response = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .devices_provision(&auth_types::AuthInstanceProvisionRequest {
            deployment_id: wire(deployment_id)?,
            identity_public_key: Some(wire(identity.public_identity_key)?),
            title: wire(args.name.as_deref().unwrap_or(id))?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    print_device_provision_result(format, &response.instance, &root_secret)
}

fn kind_noun(kind: DeploymentKind) -> &'static str {
    match kind {
        DeploymentKind::Service => "service",
        DeploymentKind::Device => "device",
    }
}

fn print_deployments<T: serde::Serialize>(
    format: OutputFormat,
    kind: DeploymentKind,
    deployments: &[T],
    with_disabled: bool,
) -> miette::Result<()> {
    output::emit(format, &json!({ "deployments": deployments }), |value| {
        let noun = kind_noun(kind);
        output::table(
            output::list(value, "deployments"),
            &[
                output::col("Name", "title"),
                output::state_col("State", "state"),
                output::id_col("Participant", "participantId"),
                output::muted("ID", "deploymentId"),
            ],
            &if with_disabled {
                format!("No {noun}s yet.")
            } else {
                format!("No enabled {noun}s. Add --disabled to include the rest.")
            },
        )?;
        if output::list(value, "deployments").is_empty() && with_disabled {
            output::next(match kind {
                DeploymentKind::Service => "trellis svc <NAME> create --source <DIR>",
                DeploymentKind::Device => "trellis dev <NAME> create --source <DIR>",
            });
        }
        Ok(())
    })
}

fn print_deployment_show_result<T: serde::Serialize>(
    format: OutputFormat,
    kind: DeploymentKind,
    deployment: &T,
) -> miette::Result<()> {
    output::emit(format, &json!({ "deployment": deployment }), |value| {
        print_deployment(kind, &value["deployment"]);
        Ok(())
    })
}

fn print_deployment(kind: DeploymentKind, deployment: &Value) {
    output::Details::new(output::field(deployment, "title"))
        .field("Kind", kind_noun(kind))
        .field("ID", output::field(deployment, "deploymentId"))
        .state("State", &output::field(deployment, "state"))
        .field("Participant", output::field(deployment, "participantId"))
        .field("Revision", output::field(deployment, "revision"))
        .field("Digest", output::field(deployment, "implementationDigest"))
        .section("Required capabilities")
        .items(
            output::list(&deployment["selection"], "requiredCapabilities")
                .iter()
                .map(|id| output::field(id, "")),
        )
        .section("Optional capabilities")
        .items(
            output::list(&deployment["selection"], "optionalCapabilities")
                .iter()
                .map(|id| output::field(id, "")),
        )
        .print();
}

fn print_service_instances_result<T: serde::Serialize>(
    format: OutputFormat,
    instances: T,
) -> miette::Result<()> {
    output::emit(format, &json!({ "instances": instances }), |value| {
        output::table(
            output::list(value, "instances"),
            &[
                output::col("Name", "principal.title"),
                output::state_col("State", "status.state"),
                output::state_col("Health", "status.health.status"),
                output::col("Build", "status.build.label"),
                output::time_col("Connected", "status.connectedAt"),
                output::time_col("Last heartbeat", "status.lastHeartbeat"),
                output::state_col("Enabled", "principal.enabled"),
                output::col("Origin", "origin.kind"),
                output::muted("ID", "instanceId"),
            ],
            "No instances. Provision one with `provision`, or issue an enrollment secret with \
            `enrollment issue`.",
        )?;
        // What a waiting instance needs and which checks fail, which the
        // table has no room for.
        for instance in output::list(value, "instances") {
            let name = output::field(instance, "principal.title");
            for check in output::list(&instance["status"]["health"], "failingChecks") {
                // The error says what went wrong; a summary stands in without one.
                let message = [
                    output::field(check, "error"),
                    output::field(check, "summary"),
                ]
                .into_iter()
                .find(|message| !message.is_empty())
                .map(|message| format!(": {message}"))
                .unwrap_or_default();
                // Instances of one service share its name, so the ID says which.
                anstream::println!(
                    "{}: check {} {}{message}",
                    output::field(instance, "instanceId"),
                    output::field(check, "name"),
                    output::state(&output::field(check, "status")),
                );
            }
            if output::field(instance, "status.health.status") == "offline"
                && output::field(instance, "status.state") != "offline"
            {
                anstream::println!(
                    "{}: heartbeats stopped; last heartbeat {}",
                    output::field(instance, "instanceId"),
                    output::field(instance, "status.lastHeartbeat"),
                );
            }
            let url = output::field(instance, "status.reviewUrl");
            if output::field(instance, "status.state") == "waitingForApproval" {
                if url.is_empty() {
                    eprintln!("{name} waits for its update to be approved; see `updates`.");
                } else {
                    eprintln!("{name} waits for its update to be approved: {url}");
                }
            }
            if output::field(instance, "status.state") == "waitingForSettings" {
                let missing = output::field(instance, "status.missingSettings");
                eprintln!("{name} waits for settings: {missing}");
            }
        }
        Ok(())
    })
}

fn print_device_instances_result<T: serde::Serialize>(
    format: OutputFormat,
    instances: T,
) -> miette::Result<()> {
    output::emit(format, &json!({ "instances": instances }), |value| {
        output::table(
            output::list(value, "instances"),
            &[
                output::col("Name", "principal.title"),
                output::state_col("Enabled", "principal.enabled"),
                output::muted("ID", "instanceId"),
                output::muted("Identity key", "identityPublicKey"),
            ],
            "No devices are provisioned. Provision one with `provision`.",
        )
    })
}

fn print_service_provision_result<T: serde::Serialize>(
    format: OutputFormat,
    instance: &T,
    generated_seed: Option<&str>,
    reused: bool,
) -> miette::Result<()> {
    if output::is_json(format) {
        return output::print_json(&json!({
            "instance": instance,
            "generatedSeed": generated_seed.is_some(),
            "instanceSeed": generated_seed,
            "reused": reused,
        }));
    }
    let value = serde_json::to_value(instance).into_diagnostic()?;
    let name = output::strong(output::field(&value, "principal.title"));
    if reused {
        output::unchanged(format!("{name} already has an instance"));
    } else {
        output::success(format!("Provisioned an instance of {name}"));
    }
    output::Details::untitled()
        .field("Instance", output::field(&value, "instanceId"))
        .field("Identity key", output::field(&value, "identityPublicKey"))
        .field("Instance seed", generated_seed.unwrap_or_default())
        .print();
    if generated_seed.is_some() {
        output::not_kept(
            "instance seed",
            "The service connects with it as this instance.",
        );
    } else if reused {
        output::hint(
            "Trellis can't recover its seed. Keep using the one you have, or pass --new for \
            another instance.",
        );
    }
    Ok(())
}

fn print_device_provision_result<T: serde::Serialize>(
    format: OutputFormat,
    instance: &T,
    root_secret: &str,
) -> miette::Result<()> {
    if output::is_json(format) {
        return output::print_json(&json!({ "instance": instance, "rootSecret": root_secret }));
    }
    let value = serde_json::to_value(instance).into_diagnostic()?;
    output::success(format!(
        "Provisioned device {}",
        output::strong(output::field(&value, "principal.title"))
    ));
    output::Details::untitled()
        .field("Instance", output::field(&value, "instanceId"))
        .field("Root secret", root_secret)
        .print();
    output::not_kept(
        "root secret",
        "Load it onto the device; it derives the device's identity from it.",
    );
    Ok(())
}

fn print_deployment_result<T: serde::Serialize>(
    format: OutputFormat,
    verb: &str,
    kind: DeploymentKind,
    deployment: &T,
) -> miette::Result<()> {
    output::emit(format, &json!({ "deployment": deployment }), |value| {
        let deployment = &value["deployment"];
        output::success(format!(
            "{verb} {} {} (revision {})",
            kind_noun(kind),
            output::strong(output::field(deployment, "title")),
            output::field(deployment, "revision")
        ));
        Ok(())
    })
}

fn ref_label(kind: DeploymentKind, id: &str) -> String {
    let prefix = match kind {
        DeploymentKind::Service => "svc",
        DeploymentKind::Device => "dev",
    };
    format!("{prefix}/{id}")
}

fn cli_idempotency_key() -> String {
    ulid::Ulid::generate().to_string()
}

/// The service deployment, or the waiting service claim, `id` names by ID or
/// title: a claim's first update belongs to the claim until it is approved.
/// Only a deployment list that answered without a match moves on to claims.
async fn resolve_update_owner(id: &str) -> miette::Result<String> {
    if let Some(deployment) = lookup_deployment_id(DeploymentKind::Service, id).await? {
        return Ok(deployment);
    }
    lookup_waiting_claim(id)
        .await?
        .ok_or_else(|| miette::miette!("deployment not found: {id}"))
}

/// The waiting service claim `id` names by ID or title, or `None` when the
/// claim list answered without one.
pub(super) async fn lookup_waiting_claim(id: &str) -> miette::Result<Option<String>> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let claims = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1()
        .service_claims_list_items(auth_types::AuthPageRequest {
            search: None,
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .try_filter(|claim| {
            std::future::ready(claim.state == auth_types::AuthServiceClaimState::Waiting)
        })
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()?
        .into_iter()
        .map(|claim| json!({ "deploymentId": claim.claim_id.to_string(), "title": claim.title }));
    // A claim ID names only the claim with that ID, never another claim's title.
    let found = if id.parse::<ulid::Ulid>().is_ok() {
        claims
            .into_iter()
            .find(|claim| claim.get("deploymentId").and_then(Value::as_str) == Some(id))
    } else {
        find_deployment_entry(claims, id)?
    };
    found
        .map(|claim| {
            claim
                .get("deploymentId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| miette::miette!("service claim has no id"))
        })
        .transpose()
}

/// Resolve a CLI deployment identifier, a deployment ID or an exact display
/// name, to a concrete deployment ID. The lookup runs on its own connection so
/// callers keep one client per connection.
pub(super) async fn resolve_deployment_id(
    kind: DeploymentKind,
    id: &str,
) -> miette::Result<String> {
    lookup_deployment_id(kind, id)
        .await?
        .ok_or_else(|| miette::miette!("deployment not found: {id}"))
}

/// The deployment ID `id` names, or `None` when the deployment list answered
/// without one. A failed list or an ambiguous name is an error.
pub(super) async fn lookup_deployment_id(
    kind: DeploymentKind,
    id: &str,
) -> miette::Result<Option<String>> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    list_deployments(&connected, id, kind)
        .await?
        .map(|deployment| {
            deployment
                .get("deploymentId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| miette::miette!("deployment has no id"))
        })
        .transpose()
}

pub(super) async fn find_deployment(
    connected: &Client,
    deployment_id: &str,
    kind: DeploymentKind,
) -> miette::Result<Value> {
    list_deployments(connected, deployment_id, kind)
        .await?
        .ok_or_else(|| miette::miette!("deployment not found: {deployment_id}"))
}

/// The deployment of `kind` that `deployment_id` names, `None` when Trellis
/// answered without one. A well-formed deployment ID is looked up by ID only:
/// it never matches another deployment's title, and a failed lookup is
/// reported as it failed.
async fn list_deployments(
    connected: &Client,
    deployment_id: &str,
    kind: DeploymentKind,
) -> miette::Result<Option<Value>> {
    use trellis_runtime_apis::apis::trellis_auth_v1::rpc::DeploymentsGetError;
    let kind = match kind {
        DeploymentKind::Service => auth_types::AuthDeploymentKind::Service,
        DeploymentKind::Device => auth_types::AuthDeploymentKind::Device,
    };
    if deployment_id.parse::<ulid::Ulid>().is_ok() {
        return match trellis_runtime_apis::apps::trellis_cli::Client::from_generated(
            connected.clone(),
        )
        .trellis_auth_v1()
        .deployments_get(&auth_types::AuthDeploymentGetRequest {
            deployment_id: wire(deployment_id)?,
            extra: Default::default(),
        })
        .await
        {
            Ok(found) if found.deployment.kind == kind => serde_json::to_value(found.deployment)
                .into_diagnostic()
                .map(Some),
            // A device's ID does not name a service, nor the other way round.
            Ok(_) => Ok(None),
            Err(trellis_rs::client::CallError::Declared(error))
                if matches!(
                    error.as_ref(),
                    DeploymentsGetError::AuthError(error)
                        if error.payload().is_ok_and(|payload| payload.code.as_ref() == "not_found")
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error).into_diagnostic(),
        };
    }
    let entries =
        trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected.clone())
            .trellis_auth_v1()
            .deployments_list_items(auth_types::AuthPageRequest {
                search: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor: None,
                    limit: Some(100),
                }),
                extra: Default::default(),
            })
            .try_filter(|item| std::future::ready(item.kind == kind))
            .try_collect::<Vec<_>>()
            .await
            .into_diagnostic()?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;
    find_deployment_entry(entries, deployment_id)
}

/// The entry `deployment_id` names by ID or exact title, `None` when none
/// does, and an error when several share the title.
fn find_deployment_entry(
    entries: impl IntoIterator<Item = Value>,
    deployment_id: &str,
) -> miette::Result<Option<Value>> {
    // The identifier may be a deployment ID or an exact display name: `svc <name> create` takes
    // a display name, so `apply`/`show`/`remove` accept the same value instead of failing with a
    // remote not-found for a name that was never an ID.
    let entries = entries.into_iter().collect::<Vec<_>>();
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.get("deploymentId").and_then(Value::as_str) == Some(deployment_id))
    {
        return Ok(Some(entry.clone()));
    }
    let mut matches = entries
        .into_iter()
        .filter(|entry| entry.get("title").and_then(Value::as_str) == Some(deployment_id))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        count => Err(miette::miette!(
            "deployment name '{deployment_id}' is ambiguous: {count} deployments \
            match; use a deployment ID"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_lookup_reaches_items_after_the_first_page() {
        let entries = (0..101).map(|index| json!({ "deploymentId": format!("svc-{index}") }));

        let deployment = find_deployment_entry(entries, "svc-100")
            .expect("deployment lookup")
            .expect("deployment found");

        assert_eq!(deployment["deploymentId"], "svc-100");
    }

    #[test]
    fn deployment_lookup_accepts_an_exact_display_name_and_rejects_ambiguity() {
        let entries = vec![
            json!({ "deploymentId": "dep_1", "title": "survey" }),
            json!({ "deploymentId": "dep_2", "title": "camera" }),
        ];
        let found = find_deployment_entry(entries.clone(), "survey")
            .expect("named lookup")
            .expect("named deployment");
        assert_eq!(found["deploymentId"], "dep_1");
        assert!(find_deployment_entry(entries, "missing")
            .expect("lookup")
            .is_none());

        let ambiguous = vec![
            json!({ "deploymentId": "dep_1", "title": "survey" }),
            json!({ "deploymentId": "dep_2", "title": "survey" }),
        ];
        let error = find_deployment_entry(ambiguous, "survey").expect_err("ambiguous name");
        assert!(error.to_string().contains("ambiguous"));
    }
}
