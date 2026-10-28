use std::io::{self, Write};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};
use trellis_idl::project::read_manifest;
use trellis_idl::CanonicalMode;
use trellis_rs::auth as authlib;
use trellis_rs::generated::Client;
use trellis_runtime_apis::apis::trellis_auth_v1::Client as AuthClient;
use trellis_runtime_apis::types as auth_types;

use crate::app::{
    connect_authenticated_cli_client, generate_session_keypair, json_value_label, wire,
};
use crate::cli::*;
use crate::output;

pub(super) async fn run_svc(format: OutputFormat, command: SvcCommand) -> miette::Result<()> {
    match (command.id, command.command) {
        (None, SvcSubcommand::List(args)) => list_services(format, &args).await,
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
        SvcResourceAction::Disable => toggle_service(format, &command.id, false).await,
        SvcResourceAction::Enable => toggle_service(format, &command.id, true).await,
        SvcResourceAction::Remove(args) => {
            remove_deployment(format, DeploymentKind::Service, &command.id, &args).await
        }
        SvcResourceAction::Instances(args) => service_instances(format, &command.id, &args).await,
        SvcResourceAction::Provision(args) => provision_service(format, &command.id, &args).await,
        SvcResourceAction::Updates => service_updates(format, &command.id).await,
        SvcResourceAction::Approve(args) => {
            decide_service_update(format, &command.id, &args, true).await
        }
        SvcResourceAction::Reject(args) => {
            decide_service_update(format, &command.id, &args, false).await
        }
    }
}

async fn list_service_updates(
    connected: &Client,
    deployment_id: &str,
) -> miette::Result<Vec<auth_types::AuthServiceUpdate>> {
    AuthClient::from_generated(connected.clone())
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
        println!("{id} has not submitted any updates.");
    }
    for update in &updates {
        print_service_update(update);
    }
    Ok(())
}

/// Print what an update asks for and gives up, in the Console's words.
fn print_service_update(update: &auth_types::AuthServiceUpdate) {
    let changes = &update.changes;
    let resource =
        |item: &auth_types::AuthServiceUpdateResource| format!("{} {}", item.kind, item.name);
    println!(
        "{}  {}  {}",
        update.update_id, update.state, update.requested_at
    );
    let section = |title: &str, lines: Vec<String>| {
        if !lines.is_empty() {
            println!("  {title}");
            for line in lines {
                println!("    {line}");
            }
        }
    };
    section(
        "Asks for:",
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
            .collect(),
    );
    section(
        "Gives up:",
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
    section(
        "Provides:",
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
    if update.state == auth_types::AuthServiceUpdateState::Pending {
        println!("  Review: {}", update.review_url);
    }
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
    let response = AuthClient::from_generated(connected)
        .service_updates_decide(&auth_types::AuthServiceUpdateDecideRequest {
            update_id: selected.update_id,
            acknowledge_shared_apis: (approve && args.acknowledge_shared_apis).then_some(true),
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
    match response.update.state {
        auth_types::AuthServiceUpdateState::Applied => {
            println!("Approved. {id} connects with its new build now.")
        }
        auth_types::AuthServiceUpdateState::Rejected => {
            println!("Rejected. {id} keeps its installed build.")
        }
        _ => println!(
            "{id} changed since it submitted this update; it will submit again when it reconnects."
        ),
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
        DevResourceAction::Disable => toggle_device(format, &id, false).await,
        DevResourceAction::Enable => toggle_device(format, &id, true).await,
        DevResourceAction::Remove(args) => {
            remove_deployment(format, DeploymentKind::Device, &id, &args).await
        }
        DevResourceAction::Instances(args) => device_instances(format, &id, &args).await,
        DevResourceAction::Provision(args) => provision_device(format, &id, &args).await,
    }
}

#[derive(Clone, Copy)]
enum DeploymentKind {
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
    let participant = match selected_participant {
        Some(id) => candidates
            .iter()
            .find(|participant| participant.identity().as_str() == id)
            .copied()
            .ok_or_else(|| {
                miette::miette!(
                    "participant '{id}' does not match; candidates: {}",
                    candidates
                        .iter()
                        .map(|candidate| candidate.identity().as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?,
        None if candidates.len() == 1 => candidates[0],
        None => {
            return Err(miette::miette!(
                "select one participant with --participant; candidates: {}",
                candidates
                    .iter()
                    .map(|candidate| candidate.identity().as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    };
    let participant_path = participant.name().to_owned();
    let participant_id = participant.identity().as_str().to_owned();
    let participant_digest = trellis_idl::participant_digest(&compiled, participant.identity())?;
    let implemented_apis = participant
        .implements()
        .iter()
        .map(ToString::to_string)
        .collect();
    let package_evidence = auth_types::AuthPackageEvidence {
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
                        &compiled,
                        id,
                        CanonicalMode::Presentation,
                    )?,
                    extra: Default::default(),
                })
            })
            .collect::<miette::Result<_>>()?,
        extra: Default::default(),
    };
    Ok(CompiledParticipantInput {
        participant_id,
        participant_digest,
        participant_path,
        package_digest: compiled.root_digest().to_owned(),
        package_evidence,
        implemented_apis,
    })
}

async fn list_services(format: OutputFormat, args: &SvcListArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployments = AuthClient::from_generated(connected.clone())
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
                    && (args.disabled || item.state == auth_types::AuthDeploymentState::Enabled),
            )
        })
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&json!({ "deployments": deployments }))?;
        return Ok(());
    }
    print_value_table(
        &serde_json::to_value(deployments).into_diagnostic()?,
        &["deploymentId", "state", "title"],
    )?;
    Ok(())
}

async fn list_devices(format: OutputFormat, args: &DevListArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployments = AuthClient::from_generated(connected.clone())
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
                    && (args.disabled || item.state == auth_types::AuthDeploymentState::Enabled),
            )
        })
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&json!({ "deployments": deployments }))?;
        return Ok(());
    }
    print_value_table(
        &serde_json::to_value(deployments).into_diagnostic()?,
        &["deploymentId", "state", "title"],
    )?;
    Ok(())
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
    let Some(source) = &args.source else {
        return create_service_claim(format, id).await;
    };
    let participant = compile_participant_input(
        source,
        args.participant.as_deref(),
        Some(trellis_idl::ParticipantKind::Service),
    )?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let response = AuthClient::from_generated(connected)
        .deployments_create(&auth_types::AuthDeploymentCreateRequest {
            kind: auth_types::AuthDeploymentKind::Service,
            title: wire(id)?,
            participant_id: participant.participant_id,
            package_evidence: participant.package_evidence,
            acknowledge_shared_apis: args.acknowledge_shared_apis.then_some(true),
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .map_err(shared_api_hint)?;
    print_deployment_result(format, "service deployment created", &response.deployment)
}

/// Create a claim the first service to connect with the generated seed fills
/// in, for a service whose IDL the administrator does not have beforehand.
async fn create_service_claim(format: OutputFormat, id: &str) -> miette::Result<()> {
    let (seed, key) = generate_session_keypair();
    let (_, connected) = connect_authenticated_cli_client().await?;
    let response = AuthClient::from_generated(connected)
        .service_claims_create(&auth_types::AuthServiceClaimCreateRequest {
            title: wire(id)?,
            instance_title: wire(id)?,
            identity_public_key: wire(key)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        return output::print_json(&json!({ "claim": response.claim, "instanceSeed": seed }));
    }
    output::print_success("service claim created");
    let claim = response.claim.claim_id.to_string();
    println!("claimId={claim}");
    output::print_info(&format!("instanceSeed={seed}"));
    output::print_info(&format!(
        "Give the seed to the service. When it connects it asks for what it needs; \
        review and approve that with `trellis svc {claim} approve`."
    ));
    Ok(())
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
    } else {
        miette::miette!("{message}")
    }
}

async fn create_device(format: OutputFormat, id: &str, args: &DevCreateArgs) -> miette::Result<()> {
    let participant = compile_participant_input(
        &args.source,
        args.participant.as_deref(),
        Some(trellis_idl::ParticipantKind::Device),
    )?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let response = AuthClient::from_generated(connected)
        .deployments_create(&auth_types::AuthDeploymentCreateRequest {
            kind: auth_types::AuthDeploymentKind::Device,
            title: wire(id)?,
            participant_id: participant.participant_id,
            package_evidence: participant.package_evidence,
            acknowledge_shared_apis: args.acknowledge_shared_apis.then_some(true),
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .map_err(shared_api_hint)?;
    print_deployment_result(format, "device deployment created", &response.deployment)
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
    let participant = compile_participant_input(
        &args.source,
        args.participant.as_deref(),
        Some(expected_kind),
    )?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, deployment_id, kind).await?;
    miette::ensure!(
        current["participantId"].as_str() == Some(&participant.participant_id),
        "deployment participant does not match the selected source participant"
    );
    let deployment_id = current["deploymentId"]
        .as_str()
        .ok_or_else(|| miette::miette!("deployment response missing deploymentId"))?;
    let revision = current["revision"]
        .as_str()
        .ok_or_else(|| miette::miette!("deployment response missing revision"))?;
    let response = AuthClient::from_generated(connected)
        .deployments_apply(&auth_types::AuthDeploymentApplyRequest {
            deployment_id: wire(deployment_id)?,
            expected_revision: wire(
                args.expected_revision
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| revision.to_owned()),
            )?,
            idempotency_key: wire(cli_idempotency_key())?,
            package_evidence: participant.package_evidence,
            acknowledge_shared_apis: args.acknowledge_shared_apis.then_some(true),
            extra: Default::default(),
        })
        .await
        .map_err(shared_api_hint)?;
    print_deployment_result(format, "deployment package applied", &response.deployment)
}

async fn toggle_service(format: OutputFormat, id: &str, enable: bool) -> miette::Result<()> {
    toggle_deployment(format, id, enable, DeploymentKind::Service).await
}

async fn toggle_deployment(
    format: OutputFormat,
    id: &str,
    enable: bool,
    kind: DeploymentKind,
) -> miette::Result<()> {
    let (_, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, id, kind).await?;
    let request = auth_types::AuthDeploymentMutationRequest {
        deployment_id: wire(&current["deploymentId"])?,
        expected_revision: wire(&current["revision"])?,
        idempotency_key: wire(cli_idempotency_key())?,
        extra: Default::default(),
    };
    let auth = AuthClient::from_generated(connected);
    let response = if enable {
        auth.deployments_enable(&request).await.into_diagnostic()?
    } else {
        auth.deployments_disable(&request).await.into_diagnostic()?
    };
    if output::is_json(format) {
        output::print_json(&json!({"deployment": response.deployment}))?;
    } else {
        print_toggle_text(kind, id, enable);
    }
    Ok(())
}

async fn toggle_device(format: OutputFormat, id: &str, enable: bool) -> miette::Result<()> {
    toggle_deployment(format, id, enable, DeploymentKind::Device).await
}

async fn remove_deployment(
    format: OutputFormat,
    kind: DeploymentKind,
    id: &str,
    args: &RemoveArgs,
) -> miette::Result<()> {
    miette::ensure!(
        !output::is_json(format) || args.force,
        "use -f with --format json to skip the interactive removal review"
    );
    let label = ref_label(kind, id);
    if !output::is_json(format) && !args.force && !prompt_for_typed_identifier(&label)? {
        return Err(miette::miette!("deployment removal cancelled"));
    }
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, id, kind).await?;
    let response = AuthClient::from_generated(connected.clone())
        .deployments_remove(&auth_types::AuthDeploymentMutationRequest {
            deployment_id: wire(&current["deploymentId"])?,
            expected_revision: wire(&current["revision"])?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    print_remove_result(format, kind, id, serde_json::to_value(response).is_ok())
}

async fn service_instances(
    format: OutputFormat,
    id: &str,
    args: &SvcInstancesArgs,
) -> miette::Result<()> {
    let deployment_id = resolve_deployment_id(DeploymentKind::Service, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let instances = AuthClient::from_generated(connected)
        .service_instances_list_items(auth_types::AuthInstancesListRequest {
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
    print_service_instances_result(format, instances)
}

async fn device_instances(
    format: OutputFormat,
    id: &str,
    args: &DevInstancesArgs,
) -> miette::Result<()> {
    let deployment_id = resolve_deployment_id(DeploymentKind::Device, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let instances = AuthClient::from_generated(connected)
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
    let deployment_id = resolve_deployment_id(DeploymentKind::Service, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let client = AuthClient::from_generated(connected);
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
    let supplied_key = args
        .instance_seed
        .as_deref()
        .map(authlib::session_public_key)
        .transpose()
        .into_diagnostic()?;
    if !args.new {
        let candidates = current
            .iter()
            .filter(|item| {
                supplied_key.as_ref().is_none_or(|key| {
                    item.identity_public_key
                        .as_ref()
                        .is_some_and(|public| public.as_ref() == key)
                })
            })
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [existing] => return print_service_provision_result(format, existing, None, true),
            [] if current.is_empty() => {}
            _ => {
                return Err(miette::miette!(
                    "existing service identity is ambiguous or different; use --new or \
                    --instance-seed to select it"
                ))
            }
        }
    }
    let (seed, key) = match (&args.instance_seed, supplied_key) {
        (Some(seed), Some(key)) => (seed.clone(), key),
        _ => generate_session_keypair(),
    };
    miette::ensure!(
        !current.iter().any(|item| item
            .identity_public_key
            .as_ref()
            .is_some_and(|public| public.as_ref() == key)),
        "identity already belongs to an existing instance"
    );
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
    print_service_provision_result(
        format,
        &response.instance,
        args.instance_seed.is_none().then_some(seed.as_str()),
        false,
    )
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
    let response = AuthClient::from_generated(connected)
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

fn print_deployment_show_result<T: serde::Serialize>(
    format: OutputFormat,
    kind: DeploymentKind,
    deployment: &T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "deployment": deployment }))?;
        return Ok(());
    }

    let value = serde_json::to_value(deployment).into_diagnostic()?;
    output::print_info(&format!(
        "ref={}",
        ref_label(kind, &value_string(&value, "deploymentId"))
    ));
    for field in [
        "title",
        "state",
        "revision",
        "participantId",
        "implementationDigest",
        "selection",
    ] {
        print_value_field(&value, field);
    }
    Ok(())
}

fn print_toggle_text(kind: DeploymentKind, id: &str, enable: bool) {
    let state = if enable { "enabled" } else { "disabled" };
    output::print_success(&format!("{state} deployment"));
    output::print_info(&format!("ref={}", ref_label(kind, id)));
}

fn print_remove_result(
    format: OutputFormat,
    kind: DeploymentKind,
    id: &str,
    success: bool,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "success": success, "deploymentId": id }))?;
        return Ok(());
    }

    if success {
        output::print_success("removed deployment");
    } else {
        output::print_info("no matching deployment removed");
    }
    output::print_info(&format!("ref={}", ref_label(kind, id)));
    Ok(())
}

fn print_service_instances_result<T: serde::Serialize>(
    format: OutputFormat,
    instances: T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "instances": instances }))?;
        return Ok(());
    }

    print_value_table(
        &serde_json::to_value(instances).into_diagnostic()?,
        &[
            "instanceId",
            "deploymentId",
            "principal.enabled",
            "principal.title",
            "revision",
        ],
    )
}

fn print_device_instances_result<T: serde::Serialize>(
    format: OutputFormat,
    instances: T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "instances": instances }))?;
        return Ok(());
    }

    print_value_table(
        &serde_json::to_value(instances).into_diagnostic()?,
        &[
            "instanceId",
            "deploymentId",
            "principal.enabled",
            "principal.title",
            "identityPublicKey",
            "revision",
        ],
    )
}

fn print_service_provision_result<T: serde::Serialize>(
    format: OutputFormat,
    instance: &T,
    generated_seed: Option<&str>,
    reused: bool,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({
            "instance": instance,
            "generatedSeed": generated_seed.is_some(),
            "instanceSeed": generated_seed,
            "reused": reused,
        }))?;
        return Ok(());
    }

    output::print_success(if reused {
        "service instance already exists"
    } else {
        "provisioned service instance"
    });
    let value = serde_json::to_value(instance).into_diagnostic()?;
    print_value_field(&value, "instanceId");
    print_value_field(&value, "deploymentId");
    print_value_field(&value, "identityPublicKey");
    if let Some(seed) = generated_seed {
        output::print_info(&format!("instanceSeed={seed}"));
    } else if reused {
        output::print_info(
            "Trellis cannot recover the original private seed; reuse the existing \
            provisioned identity",
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
        output::print_json(&json!({ "instance": instance, "rootSecret": root_secret }))?;
        return Ok(());
    }

    output::print_success("provisioned device instance");
    let value = serde_json::to_value(instance).into_diagnostic()?;
    print_value_field(&value, "instanceId");
    print_value_field(&value, "deploymentId");
    print_value_field(&value, "identityPublicKey");
    output::print_info(&format!("rootSecret={root_secret}"));
    Ok(())
}

fn print_value_table(value: &Value, columns: &[&str]) -> miette::Result<()> {
    let rows = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    columns
                        .iter()
                        .map(|column| value_string(item, column))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    println!("{}", output::table(columns, rows));
    Ok(())
}

fn print_value_field(value: &Value, field: &str) {
    let rendered = value_string(value, field);
    if !rendered.is_empty() {
        output::print_info(&format!("{field}={rendered}"));
    }
}

fn value_string(value: &Value, field: &str) -> String {
    let value = field
        .split('.')
        .try_fold(value, |value, field| value.get(field));
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(values)) => values
            .iter()
            .map(json_value_label)
            .collect::<Vec<_>>()
            .join(","),
        Some(value @ Value::Object(_)) => json_value_label(value),
        Some(Value::Null) | None => String::new(),
    }
}

fn print_deployment_result<T: serde::Serialize>(
    format: OutputFormat,
    message: &str,
    deployment: &T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "deployment": deployment }))?;
    } else {
        output::print_success(message);
    }
    Ok(())
}

fn ref_label(kind: DeploymentKind, id: &str) -> String {
    let prefix = match kind {
        DeploymentKind::Service => "svc",
        DeploymentKind::Device => "dev",
    };
    format!("{prefix}/{id}")
}

fn prompt_for_typed_identifier(identifier: &str) -> miette::Result<bool> {
    print!("Type {identifier} to confirm: ");
    io::stdout().flush().into_diagnostic()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line).into_diagnostic()?;
    Ok(line.trim() == identifier)
}

fn cli_idempotency_key() -> String {
    ulid::Ulid::new().to_string()
}

/// Resolve a CLI deployment identifier to a concrete deployment ID. The identifier may be a
/// deployment ID or an exact display name. The lookup runs on its own connection so callers keep
/// one client per connection.
/// The service deployment, or the waiting service claim, `id` names by ID or
/// title: a claim's first update belongs to the claim until it is approved.
async fn resolve_update_owner(id: &str) -> miette::Result<String> {
    if let Ok(deployment) = resolve_deployment_id(DeploymentKind::Service, id).await {
        return Ok(deployment);
    }
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let claims = AuthClient::from_generated(connected)
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
    find_deployment_entry(claims, id)?
        .get("deploymentId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| miette::miette!("service claim has no id"))
}

async fn resolve_deployment_id(kind: DeploymentKind, id: &str) -> miette::Result<String> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    find_deployment(&connected, id, kind)
        .await?
        .get("deploymentId")
        .and_then(Value::as_str)
        .ok_or_else(|| miette::miette!("deployment has no id"))
        .map(str::to_owned)
}

async fn find_deployment(
    connected: &Client,
    deployment_id: &str,
    kind: DeploymentKind,
) -> miette::Result<Value> {
    let kind = match kind {
        DeploymentKind::Service => auth_types::AuthDeploymentKind::Service,
        DeploymentKind::Device => auth_types::AuthDeploymentKind::Device,
    };
    let entries = AuthClient::from_generated(connected.clone())
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

fn find_deployment_entry(
    entries: impl IntoIterator<Item = Value>,
    deployment_id: &str,
) -> miette::Result<Value> {
    // The identifier may be a deployment ID or an exact display name: `svc <name> create` takes
    // a display name, so `apply`/`show`/`remove` accept the same value instead of failing with a
    // remote not-found for a name that was never an ID.
    let entries = entries.into_iter().collect::<Vec<_>>();
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.get("deploymentId").and_then(Value::as_str) == Some(deployment_id))
    {
        return Ok(entry.clone());
    }
    let matches = entries
        .into_iter()
        .filter(|entry| entry.get("title").and_then(Value::as_str) == Some(deployment_id))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(miette::miette!("deployment not found: {deployment_id}")),
        1 => Ok(matches.into_iter().next().expect("exactly one match")),
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

        let deployment = find_deployment_entry(entries, "svc-100").expect("deployment found");

        assert_eq!(deployment["deploymentId"], "svc-100");
    }

    #[test]
    fn deployment_lookup_accepts_an_exact_display_name_and_rejects_ambiguity() {
        let entries = vec![
            json!({ "deploymentId": "dep_1", "title": "survey" }),
            json!({ "deploymentId": "dep_2", "title": "camera" }),
        ];
        let found = find_deployment_entry(entries.clone(), "survey").expect("named lookup");
        assert_eq!(found["deploymentId"], "dep_1");
        assert!(find_deployment_entry(entries, "missing").is_err());

        let ambiguous = vec![
            json!({ "deploymentId": "dep_1", "title": "survey" }),
            json!({ "deploymentId": "dep_2", "title": "survey" }),
        ];
        let error = find_deployment_entry(ambiguous, "survey").expect_err("ambiguous name");
        assert!(error.to_string().contains("ambiguous"));
    }
}
