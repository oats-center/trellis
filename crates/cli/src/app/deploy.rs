use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};
use trellis_idl::project::read_manifest;
use trellis_idl::CanonicalMode;
use trellis_rs::auth as authlib;
use trellis_rs::generated::Client;
use trellis_runtime_apis::apis::trellis_auth_v1::rpc::DeploymentsApplyError;
use trellis_runtime_apis::apis::trellis_auth_v1::Client as AuthClient;
use trellis_runtime_apis::types as auth_types;

use crate::app::{
    connect_authenticated_cli_client, generate_session_keypair, json_value_label, wire,
};
use crate::cli::*;
use crate::output;

const DEVICE_NAME_METADATA_KEY: &str = "name";
const DEVICE_SERIAL_METADATA_KEY: &str = "serialNumber";
const DEVICE_MODEL_METADATA_KEY: &str = "modelNumber";

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
        SvcResourceAction::Create(_) => create_service(format, &command.id).await,
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
    }
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
        DevResourceAction::Activations(command) => dev_activations(format, &id, command).await,
        DevResourceAction::Reviews(command) => dev_reviews(format, &id, command).await,
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
}

pub fn compile_participant_input(
    source: &std::path::Path,
    selected_participant: Option<&str>,
    expected_kind: Option<trellis_idl::ParticipantKind>,
    _approved_capabilities: &[String],
    _approved_resources: &[String],
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
    let package_evidence = auth_types::AuthPackageEvidence {
        root_package: compiled.root().as_str().to_owned(),
        root_digest: compiled.root_digest().to_owned(),
        packages: compiled
            .packages()
            .iter()
            .map(|(id, package)| {
                Ok(auth_types::AuthPackageSourceEvidence {
                    name: id.as_str().to_owned(),
                    version: package.version().to_string(),
                    digest: compiled
                        .digest(id)
                        .expect("compiled package digest")
                        .to_owned(),
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
    })
}

async fn list_services(format: OutputFormat, args: &SvcListArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployments = AuthClient::from_generated(connected.clone())
        .deployments_list_items(auth_types::AuthDeploymentsListRequest {
            kind: Some(auth_types::AuthDeploymentsListRequestKind::Service),
            state: (!args.disabled).then_some(auth_types::AuthDeploymentsListRequestState::Active),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
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
        &["deploymentId", "state", "displayName"],
    )?;
    Ok(())
}

async fn list_devices(format: OutputFormat, args: &DevListArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployments = AuthClient::from_generated(connected.clone())
        .deployments_list_items(auth_types::AuthDeploymentsListRequest {
            kind: Some(auth_types::AuthDeploymentsListRequestKind::Device),
            state: (!args.disabled).then_some(auth_types::AuthDeploymentsListRequestState::Active),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
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
        &["deploymentId", "state", "displayName"],
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

async fn create_service(format: OutputFormat, id: &str) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployment = AuthClient::from_generated(connected.clone())
        .deployments_create(&auth_types::AuthDeploymentsCreateRequest {
            kind: auth_types::AuthDeploymentsCreateRequestKind::Service,
            display_name: wire(id)?,
            participant_id: wire(None::<String>)?,
            expires_at: wire(None::<String>)?,
            requires_device_delegation: false,
            review_mode: wire(None::<String>)?,
            portal_id: wire(None::<String>)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .deployment;
    print_deployment_result(format, "service deployment created", &deployment)
}

async fn create_device(format: OutputFormat, id: &str, args: &DevCreateArgs) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let deployment = AuthClient::from_generated(connected.clone())
        .deployments_create(&auth_types::AuthDeploymentsCreateRequest {
            kind: auth_types::AuthDeploymentsCreateRequestKind::Device,
            display_name: wire(id)?,
            participant_id: wire(None::<String>)?,
            expires_at: wire(None::<String>)?,
            requires_device_delegation: args.requires_device_delegation,
            review_mode: wire(Some(args.review_mode.as_wire_value()))?,
            portal_id: wire(None::<String>)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .deployment;
    print_deployment_result(format, "device deployment created", &deployment)
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
        &args.approve_capability,
        &args.approve_resource,
    )?;
    // Resolve the identifier to a concrete deployment ID; a display name is accepted too.
    let resolved_id = resolve_deployment_id(kind, deployment_id).await?;
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let current = AuthClient::from_generated(connected.clone())
        .deployments_get(&auth_types::AuthDeploymentsGetRequest {
            deployment_id: wire(resolved_id.clone())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    if current.deployment.kind.to_string()
        != match kind {
            DeploymentKind::Service => "service",
            DeploymentKind::Device => "device",
        }
    {
        return Err(miette::miette!("deployment kind does not match command"));
    }
    let current_participant_id = wire::<Option<String>>(&current.deployment.participant_id)?;
    if current_participant_id
        .as_deref()
        .is_some_and(|id| id != participant.participant_id)
    {
        return Err(miette::miette!(
            "deployment is assigned to participant '{}', not '{}'",
            current_participant_id.as_deref().unwrap_or_default(),
            participant.participant_id
        ));
    }
    let binding = wire::<Option<Value>>(&current.binding)?;
    let expected_revision = args.expected_revision.unwrap_or_else(|| {
        binding
            .as_ref()
            .and_then(|binding| binding.get("revision"))
            .and_then(Value::as_str)
            .and_then(|revision| revision.parse().ok())
            .unwrap_or(0)
    });
    let mut request = auth_types::AuthDeploymentsApplyRequest {
        approval: None,
        deployment_id: wire(resolved_id.clone())?,
        expected_revision: wire(expected_revision.to_string())?,
        idempotency_key: wire(cli_idempotency_key())?,
        package_evidence: participant.package_evidence,
        participant_path: participant.participant_path,
        package_digest: participant.package_digest,
        extra: Default::default(),
    };
    let client = AuthClient::from_generated(connected.clone());
    let consent = match client.deployments_apply(&request).await {
        Ok(response) => {
            return print_apply_response(
                format,
                &resolved_id,
                &participant.participant_id,
                &participant.participant_digest,
                response,
            )
        }
        Err(error) => approval_required(&error).ok_or(error).into_diagnostic()?,
    };
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(&consent).into_diagnostic()?)?;
    } else {
        output::print_info("server-computed deployment consent request:");
        output::print_json(&serde_json::to_value(&consent).into_diagnostic()?)?;
    }
    if let Some(digest) = &args.confirm_digest {
        miette::ensure!(
            digest == &consent.decision_digest,
            "--confirm-digest does not match the displayed request"
        );
    }
    let approve_all = args.yes
        || (args.approve_capability.is_empty()
            && args.approve_resource.is_empty()
            && io::stdin().is_terminal());
    if !args.yes
        && io::stdin().is_terminal()
        && !prompt_for_typed_identifier(&consent.decision_digest)?
    {
        return Err(miette::miette!("deployment approval cancelled"));
    }
    if !args.yes
        && !io::stdin().is_terminal()
        && args.approve_capability.is_empty()
        && args.approve_resource.is_empty()
        && !args.approve_companion
    {
        return Err(miette::miette!(
            "approval_required: rerun with explicit approval selectors or --yes"
        ));
    }
    request.approval = Some(approval_from_consent(&consent, args, approve_all));
    let response = match client.deployments_apply(&request).await {
        Ok(response) => response,
        Err(error)
            if approval_required(&error)
                .is_some_and(|current| current.decision_digest == consent.decision_digest) =>
        {
            client.deployments_apply(&request).await.into_diagnostic()?
        }
        Err(error) => return Err(error).into_diagnostic(),
    };
    print_apply_response(
        format,
        &resolved_id,
        &participant.participant_id,
        &participant.participant_digest,
        response,
    )
}

fn approval_from_consent(
    consent: &auth_types::ConsentRequest,
    args: &ApplyArgs,
    approve_all: bool,
) -> auth_types::Approval {
    let approved_capabilities = consent
        .capabilities
        .iter()
        .filter(|capability| {
            capability.eligible
                && (approve_all
                    || capability.already_approved
                    || args.approve_capability.contains(&capability.id))
        })
        .map(|capability| auth_types::ApprovedCapability {
            id: capability.id.clone(),
            consent_digest: capability.consent_digest.clone(),
            extra: Default::default(),
        })
        .collect();
    let approved_resources = consent
        .resources
        .iter()
        .filter(|resource| {
            resource.eligible
                && (approve_all
                    || resource.already_approved
                    || args.approve_resource.contains(&resource.name))
        })
        .map(|resource| auth_types::ApprovedResource {
            kind: resource.kind.clone(),
            name: resource.name.clone(),
            commitment: resource.requested_commitment.clone(),
            extra: Default::default(),
        })
        .collect();
    auth_types::Approval {
        approved_capabilities,
        approved_resources,
        companion_approved: consent.companion.is_some() && (approve_all || args.approve_companion),
        decision_digest: consent.decision_digest.clone(),
        delegation_ceiling: None,
        expected_grant_revision: consent.expected_grant_revision,
        installed_revision: consent.installed_revision,
        mode: auth_types::ApprovalMode::Capabilities,
        extra: Default::default(),
    }
}

fn approval_required(
    error: &trellis_rs::client::CallError<DeploymentsApplyError>,
) -> Option<auth_types::ConsentRequest> {
    let trellis_rs::client::CallError::Declared(error) = error else {
        return None;
    };
    let DeploymentsApplyError::AuthError(error) = error.as_ref() else {
        return None;
    };
    serde_json::from_value(error.error.extra.get("consentRequest")?.clone()).ok()
}

fn print_apply_response(
    format: OutputFormat,
    deployment_id: &str,
    participant_id: &str,
    participant_digest: &str,
    response: auth_types::AuthDeploymentsApplyResponse,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&serde_json::to_value(response).into_diagnostic()?)?;
    } else {
        output::print_success("deployment participant applied");
        output::print_info(&format!("deploymentId={deployment_id}"));
        output::print_info(&format!("participantId={participant_id}"));
        output::print_info(&format!("participantDigest={}", participant_digest));
        output::print_json(&response.binding)?;
    }
    Ok(())
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
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let current = find_deployment(&connected, id, kind).await?;
    let expected_version = current
        .get("version")
        .and_then(Value::as_str)
        .and_then(|version| version.parse::<i64>().ok())
        .ok_or_else(|| miette::miette!("deployment response missing version"))?;
    let auth_client = AuthClient::from_generated(connected.clone());
    let deployment = if enable {
        serde_json::to_value(
            auth_client
                .deployments_enable(&auth_types::AuthDeploymentsEnableRequest {
                    deployment_id: wire(id)?,
                    expected_version: wire(expected_version.to_string())?,
                    reason: wire(None::<String>)?,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?
                .deployment,
        )
        .into_diagnostic()?
    } else {
        serde_json::to_value(
            auth_client
                .deployments_disable(&auth_types::AuthDeploymentsDisableRequest {
                    deployment_id: wire(id)?,
                    expected_version: wire(expected_version.to_string())?,
                    reason: wire(None::<String>)?,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?
                .deployment,
        )
        .into_diagnostic()?
    };
    print_toggle_service_result(format, id, enable, &deployment)
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
    let expected_version = current
        .get("version")
        .and_then(Value::as_str)
        .and_then(|version| version.parse::<i64>().ok())
        .ok_or_else(|| miette::miette!("deployment response missing version"))?;
    let response = AuthClient::from_generated(connected.clone())
        .deployments_remove(&auth_types::AuthDeploymentsRemoveRequest {
            deployment_id: wire(id)?,
            expected_version: wire(expected_version.to_string())?,
            reason: wire(None::<String>)?,
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
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let instances = AuthClient::from_generated(connected.clone())
        .service_instances_list(&auth_types::AuthServiceInstancesListRequest {
            deployment_id: Some(wire(id)?),
            state: (!args.disabled)
                .then_some(auth_types::AuthServiceInstancesListRequestState::Active),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .items;
    print_service_instances_result(format, instances)
}

async fn device_instances(
    format: OutputFormat,
    id: &str,
    args: &DevInstancesArgs,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let instances = AuthClient::from_generated(connected.clone())
        .devices_list(&auth_types::AuthDevicesListRequest {
            deployment_id: Some(wire(id)?),
            state: args.state.map(|state| match state {
                DeviceInstanceState::Registered => auth_types::AuthDevicesListRequestState::Pending,
                DeviceInstanceState::Activated => auth_types::AuthDevicesListRequestState::Active,
                DeviceInstanceState::Disabled => auth_types::AuthDevicesListRequestState::Disabled,
                DeviceInstanceState::Revoked => auth_types::AuthDevicesListRequestState::Revoked,
            }),
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .items;
    print_device_instances_result(format, instances)
}

async fn provision_service(
    format: OutputFormat,
    id: &str,
    args: &SvcProvisionArgs,
) -> miette::Result<()> {
    let resolved_id = resolve_deployment_id(DeploymentKind::Service, id).await?;
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let (instance_seed, instance_key, generated_seed) = if let Some(seed) = &args.instance_seed {
        let session_key = authlib::session_public_key(seed).into_diagnostic()?;
        (seed.clone(), session_key, false)
    } else {
        let (seed, key) = generate_session_keypair();
        (seed, key, true)
    };
    let instance = AuthClient::from_generated(connected.clone())
        .service_instances_provision(&auth_types::AuthServiceInstancesProvisionRequest {
            deployment_id: wire(resolved_id.clone())?,
            instance_id: wire(Some(format!("inst_{}", &instance_key[..16])))?,
            identity_public_key: wire(instance_key)?,
            participant_id: wire(None::<String>)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?
        .instance;
    print_service_provision_result(format, &instance, generated_seed, &instance_seed)
}

async fn provision_device(
    format: OutputFormat,
    id: &str,
    args: &DevProvisionArgs,
) -> miette::Result<()> {
    let resolved_id = resolve_deployment_id(DeploymentKind::Device, id).await?;
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let seed: [u8; 32] = rand::random();
    let root_secret = URL_SAFE_NO_PAD.encode(seed);
    let identity = authlib::derive_device_identity(&seed).into_diagnostic()?;
    let _metadata = build_device_metadata(args)?;
    let instance = AuthClient::from_generated(connected.clone())
        .devices_provision(&auth_types::AuthDevicesProvisionRequest {
            deployment_id: wire(resolved_id.clone())?,
            instance_id: wire(None::<String>)?,
            identity_public_key: wire(Some(identity.public_identity_key))?,
            participant_id: wire(None::<String>)?,
            idempotency_key: wire(cli_idempotency_key())?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    print_device_provision_result(format, &instance, &root_secret)
}

async fn dev_activations(
    format: OutputFormat,
    deployment_id: &str,
    command: DevActivationsCommand,
) -> miette::Result<()> {
    match command {
        DevActivationsCommand::List(args) => {
            let (_state, connected) = connect_authenticated_cli_client().await?;
            let activations = AuthClient::from_generated(connected.clone())
                .devices_list(&auth_types::AuthDevicesListRequest {
                    deployment_id: Some(wire(deployment_id)?),
                    state: args.state.map(|state| match state {
                        DeviceActivationState::Activated => {
                            auth_types::AuthDevicesListRequestState::Active
                        }
                        DeviceActivationState::Revoked => {
                            auth_types::AuthDevicesListRequestState::Revoked
                        }
                    }),
                    page: Some(trellis_runtime_apis::CursorQuery {
                        cursor: None,
                        limit: Some(100),
                    }),
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?
                .items;
            let activations = activations
                .into_iter()
                .filter(|entry| {
                    args.instance
                        .as_deref()
                        .is_none_or(|id| entry.instance_id.as_ref() == id)
                })
                .collect::<Vec<_>>();
            print_device_activations_result(format, activations)
        }
        DevActivationsCommand::Revoke(args) => {
            let (_state, connected) = connect_authenticated_cli_client().await?;
            let devices = AuthClient::from_generated(connected.clone())
                .devices_list(&auth_types::AuthDevicesListRequest {
                    deployment_id: Some(wire(deployment_id)?),
                    state: None,
                    page: Some(trellis_runtime_apis::CursorQuery {
                        cursor: None,
                        limit: Some(100),
                    }),
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?
                .items;
            let device = devices
                .into_iter()
                .find(|device| device.instance_id.as_ref() == args.instance_id)
                .ok_or_else(|| miette::miette!("device not found: {}", args.instance_id))?;
            AuthClient::from_generated(connected.clone())
                .devices_disable(&auth_types::AuthDevicesDisableRequest {
                    instance_id: wire(&args.instance_id)?,
                    expected_version: wire(device.version)?,
                    reason: wire(Some("device activation revoked by CLI"))?,
                    idempotency_key: wire(cli_idempotency_key())?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            let success = true;
            print_revoke_activation_result(format, &args.instance_id, success)
        }
    }
}

async fn dev_reviews(
    format: OutputFormat,
    deployment_id: &str,
    command: DevReviewsCommand,
) -> miette::Result<()> {
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth_client = AuthClient::from_generated(connected.clone());
    match command {
        DevReviewsCommand::List(args) => {
            let reviews = auth_client
                .device_user_authorities_reviews_list(
                    &auth_types::AuthDeviceUserAuthoritiesReviewsListRequest { deployment_id: Some(wire(deployment_id)?),
                    state: args.state.map(|state| match state {
                        DeviceReviewState::Pending => auth_types::AuthDeviceUserAuthoritiesReviewsListRequestState::Pending,
                        DeviceReviewState::Approved => auth_types::AuthDeviceUserAuthoritiesReviewsListRequestState::Approved,
                        DeviceReviewState::Rejected => auth_types::AuthDeviceUserAuthoritiesReviewsListRequestState::Rejected,
                    }),
                    page: Some(trellis_runtime_apis::CursorQuery { cursor: None, limit: Some(100) }),     extra: Default::default(),
                    },
                )
                .await
                .into_diagnostic()?.items;
            let reviews = reviews
                .into_iter()
                .filter(|review| {
                    args.instance
                        .as_deref()
                        .is_none_or(|id| review.instance_id.as_ref() == id)
                })
                .collect::<Vec<_>>();
            print_device_reviews_result(format, reviews)
        }
        DevReviewsCommand::Approve(args) => {
            review_decide(format, auth_client, &args, "approve").await
        }
        DevReviewsCommand::Reject(args) => {
            review_decide(format, auth_client, &args, "reject").await
        }
    }
}

async fn review_decide(
    format: OutputFormat,
    auth_client: AuthClient,
    args: &DevReviewDecisionArgs,
    decision: &str,
) -> miette::Result<()> {
    let response = auth_client
        .device_user_authorities_reviews_list(
            &auth_types::AuthDeviceUserAuthoritiesReviewsListRequest {
                deployment_id: None,
                state: None,
                page: Some(trellis_runtime_apis::CursorQuery {
                    cursor: None,
                    limit: Some(100),
                }),
                extra: Default::default(),
            },
        )
        .await
        .into_diagnostic()?
        .items
        .into_iter()
        .find(|review| review.review_id.as_ref() == args.review_id)
        .ok_or_else(|| miette::miette!("device review not found: {}", args.review_id))?;
    let response = auth_client
        .device_user_authorities_reviews_decide(
            &auth_types::AuthDeviceUserAuthoritiesReviewsDecideRequest {
                review_id: wire(&args.review_id)?,
                decision: match decision {
                    "approve" => {
                        auth_types::AuthDeviceUserAuthoritiesReviewsDecideRequestDecision::Approve
                    }
                    _ => auth_types::AuthDeviceUserAuthoritiesReviewsDecideRequestDecision::Reject,
                },
                expected_version: wire(response.version)?,
                reason: wire(&args.reason)?,
                idempotency_key: wire(cli_idempotency_key())?,
                extra: Default::default(),
            },
        )
        .await
        .into_diagnostic()?;
    if output::is_json(format) {
        output::print_json(&response)?;
    } else {
        let message = match decision {
            "approve" => "approved device review",
            "reject" => "rejected device review",
            _ => "updated device review",
        };
        output::print_success(message);
        output::print_info(&format!("reviewId={}", args.review_id));
    }
    Ok(())
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
    print_value_field(&value, "disabled");
    print_value_field(&value, "namespaces");
    print_value_field(&value, "reviewMode");
    Ok(())
}

fn print_toggle_service_result<T: serde::Serialize>(
    format: OutputFormat,
    id: &str,
    enable: bool,
    deployment: &T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "deployment": deployment }))?;
        return Ok(());
    }

    print_toggle_text(DeploymentKind::Service, id, enable);
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
        &["instanceId", "deploymentId", "disabled"],
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
            "state",
            "publicIdentityKey",
            "name",
            "serialNumber",
            "modelNumber",
        ],
    )
}

fn print_service_provision_result<T: serde::Serialize>(
    format: OutputFormat,
    instance: &T,
    generated_seed: bool,
    instance_seed: &str,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(
            &json!({ "instance": instance, "generatedSeed": generated_seed, "instanceSeed": generated_seed.then_some(instance_seed) }),
        )?;
        return Ok(());
    }

    output::print_success("provisioned service instance");
    let value = serde_json::to_value(instance).into_diagnostic()?;
    print_value_field(&value, "instanceId");
    print_value_field(&value, "deploymentId");
    print_value_field(&value, "instanceKey");
    if generated_seed {
        output::print_info(&format!("instanceSeed={instance_seed}"));
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
    print_value_field(&value, "publicIdentityKey");
    output::print_info(&format!("rootSecret={root_secret}"));
    Ok(())
}

fn print_device_activations_result<T: serde::Serialize>(
    format: OutputFormat,
    activations: T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "activations": activations }))?;
        return Ok(());
    }

    print_value_table(
        &serde_json::to_value(activations).into_diagnostic()?,
        &[
            "instanceId",
            "deploymentId",
            "state",
            "activatedAt",
            "revokedAt",
        ],
    )
}

fn print_revoke_activation_result(
    format: OutputFormat,
    instance_id: &str,
    success: bool,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "success": success, "instanceId": instance_id }))?;
        return Ok(());
    }

    if success {
        output::print_success("revoked device activation");
    } else {
        output::print_info("no matching activation revoked");
    }
    output::print_info(&format!("instanceId={instance_id}"));
    Ok(())
}

fn print_device_reviews_result<T: serde::Serialize>(
    format: OutputFormat,
    reviews: T,
) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "reviews": reviews }))?;
        return Ok(());
    }

    print_value_table(
        &serde_json::to_value(reviews).into_diagnostic()?,
        &[
            "reviewId",
            "instanceId",
            "deploymentId",
            "state",
            "createdAt",
        ],
    )
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
    match value.get(field) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(values)) => values
            .iter()
            .map(json_value_label)
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => value.get(field).map(json_value_label).unwrap_or_default(),
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

fn build_device_metadata(
    args: &DevProvisionArgs,
) -> miette::Result<Option<BTreeMap<String, String>>> {
    let mut metadata = BTreeMap::new();
    if let Some(name) = &args.name {
        metadata.insert(DEVICE_NAME_METADATA_KEY.to_string(), name.clone());
    }
    if let Some(serial_number) = &args.serial_number {
        metadata.insert(
            DEVICE_SERIAL_METADATA_KEY.to_string(),
            serial_number.clone(),
        );
    }
    if let Some(model_number) = &args.model_number {
        metadata.insert(DEVICE_MODEL_METADATA_KEY.to_string(), model_number.clone());
    }
    for entry in &args.metadata {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(miette::miette!("metadata entries must use KEY=VALUE"));
        };
        if key.is_empty() {
            return Err(miette::miette!("metadata key must not be empty"));
        }
        metadata.insert(key.to_string(), value.to_string());
    }
    Ok((!metadata.is_empty()).then_some(metadata))
}

fn cli_idempotency_key() -> String {
    ulid::Ulid::new().to_string()
}

/// Resolve a CLI deployment identifier to a concrete deployment ID. The identifier may be a
/// deployment ID or an exact display name. The lookup runs on its own connection so callers keep
/// one client per connection.
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
        DeploymentKind::Service => auth_types::AuthDeploymentsListRequestKind::Service,
        DeploymentKind::Device => auth_types::AuthDeploymentsListRequestKind::Device,
    };
    let entries = AuthClient::from_generated(connected.clone())
        .deployments_list_items(auth_types::AuthDeploymentsListRequest {
            kind: Some(kind),
            state: None,
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
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
        .filter(|entry| entry.get("displayName").and_then(Value::as_str) == Some(deployment_id))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(miette::miette!("deployment not found: {deployment_id}")),
        1 => Ok(matches.into_iter().next().expect("exactly one match")),
        count => Err(miette::miette!(
            "deployment name '{deployment_id}' is ambiguous: {count} deployments match; use a deployment ID"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trellis_rs::client::CallError;
    use trellis_rs::generated::SerializableErrorData;
    use trellis_runtime_apis::apis::trellis_auth_v1::errors::AuthError;

    #[test]
    fn deployment_lookup_reaches_items_after_the_first_page() {
        let entries = (0..101).map(|index| json!({ "deploymentId": format!("svc-{index}") }));

        let deployment = find_deployment_entry(entries, "svc-100").expect("deployment found");

        assert_eq!(deployment["deploymentId"], "svc-100");
    }

    #[test]
    fn deployment_lookup_accepts_an_exact_display_name_and_rejects_ambiguity() {
        let entries = vec![
            json!({ "deploymentId": "dep_1", "displayName": "survey" }),
            json!({ "deploymentId": "dep_2", "displayName": "camera" }),
        ];
        let found = find_deployment_entry(entries.clone(), "survey").expect("named lookup");
        assert_eq!(found["deploymentId"], "dep_1");
        assert!(find_deployment_entry(entries, "missing").is_err());

        let ambiguous = vec![
            json!({ "deploymentId": "dep_1", "displayName": "survey" }),
            json!({ "deploymentId": "dep_2", "displayName": "survey" }),
        ];
        let error = find_deployment_entry(ambiguous, "survey").expect_err("ambiguous name");
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn deployment_apply_plan_builds_executable_selected_approval() {
        let consent: auth_types::ConsentRequest = serde_json::from_value(json!({
            "capabilities": [
                { "alreadyApproved": false, "consentDigest": "cap-digest", "consequence": "", "description": "", "eligible": true, "id": "api::write", "required": true, "title": "Write" },
                { "alreadyApproved": false, "consentDigest": "denied", "consequence": "", "description": "", "eligible": false, "id": "api::denied", "required": false, "title": "Denied" }
            ],
            "companion": null,
            "decisionDigest": "decision",
            "expectedGrantRevision": "4",
            "installedRevision": "7",
            "packageDigest": "package",
            "participantId": "acme.service@v1",
            "resources": []
        }))
        .expect("valid consent request");
        let error = CallError::Declared(Box::new(DeploymentsApplyError::AuthError(AuthError {
            error: SerializableErrorData {
                id: "error-id".to_string(),
                error_type: "trellis.auth@v1::AuthError".to_string(),
                message: "approval required".to_string(),
                context: None,
                trace_id: None,
                extra: serde_json::Map::from_iter([(
                    "consentRequest".to_string(),
                    serde_json::to_value(&consent).expect("serialize consent"),
                )]),
            },
        })));
        let plan = approval_required(&error).expect("extract server plan");
        let args = ApplyArgs {
            source: "project".into(),
            participant: None,
            approve_capability: vec!["api::write".to_string()],
            approve_resource: vec![],
            approve_companion: false,
            confirm_digest: Some("decision".to_string()),
            yes: false,
            expected_revision: None,
        };

        let approval = approval_from_consent(&plan, &args, false);

        assert_eq!(approval.decision_digest, "decision");
        assert_eq!(approval.expected_grant_revision.to_string(), "4");
        assert_eq!(approval.installed_revision.to_string(), "7");
        assert_eq!(approval.approved_capabilities.len(), 1);
        assert_eq!(approval.approved_capabilities[0].id, "api::write");
        assert!(!approval.companion_approved);

        let with_companion = approval_from_consent(
            &auth_types::ConsentRequest {
                companion: Some(auth_types::ConsentCompanion {
                    capabilities: Vec::new(),
                    kind: auth_types::ResourceOwnerKind::Device,
                    participant_id: "acme.device@v1".to_string(),
                    required: false,
                    resources: Vec::new(),
                    extra: Default::default(),
                }),
                ..consent
            },
            &args,
            true,
        );
        assert!(with_companion.companion_approved);

        let without_companion = approval_from_consent(&plan, &args, true);
        assert!(!without_companion.companion_approved);
    }
}
