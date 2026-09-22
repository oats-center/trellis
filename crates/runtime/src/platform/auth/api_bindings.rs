//! API binding resolution and cheap current-binding reads.
//!
//! Read-only binding lookup never compiles evidence or enumerates deployments.
//! Provider selection reuses the store's immutable compiled evidence and
//! compatibility results while keeping every authority decision in the
//! existing issuance and admission paths.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use super::compiled_evidence::CompiledInstalledEvidence;
use super::{
    AuthorizationStateError, DeploymentProfileState, DeploymentRepository, GrantRepository,
    ParticipantBindingRecord,
};
use crate::telemetry::{record_duration, DurationMetric, Outcome};
use trellis_idl::{InteractionSelection, PackageGraph};

/// Resolve the current API bindings for a participant without compiling
/// evidence: self bindings, implicit Events, and one batch binding read.
///
/// # Errors
///
/// Returns [`AuthorizationStateError::NotAuthorized`] when a required external
/// binding is absent and the participant owns no provider deployment.
#[tracing::instrument(
    name = "trellis.auth.current_api_bindings",
    skip_all,
    fields(trellis.surface = "auth", trellis.operation = "current_api_bindings")
)]
pub(crate) async fn current_api_bindings<R>(
    repository: &R,
    participant: &ParticipantBindingRecord,
    provider_deployment_id: Option<&str>,
) -> Result<BTreeMap<String, trellis_rs::client::AuthorizationApiBinding>, AuthorizationStateError>
where
    R: GrantRepository + Send + Sync,
{
    let total_started = Instant::now();
    let result = async {
        let mut bindings = BTreeMap::new();
        for api_id in participant.projection.implemented_apis.keys() {
            bindings.insert(
                api_id.clone(),
                trellis_rs::client::AuthorizationApiBinding {
                    provider_deployment_id: provider_deployment_id
                        .ok_or(AuthorizationStateError::NotAuthorized)?
                        .to_owned(),
                },
            );
        }
        let mut expected = participant
            .projection
            .referenced_apis
            .keys()
            .filter(|api_id| {
                !participant
                    .projection
                    .implemented_apis
                    .contains_key(*api_id)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if participant.projection.resources.values().any(|resource| {
            resource.kind == trellis_protocol::ParticipantResourceKind::EventConsumer
        }) {
            expected.insert(trellis_runtime_apis::apis::trellis_events_v1::API_ID.to_owned());
            expected.retain(|api_id| !participant.projection.implemented_apis.contains_key(api_id));
        }
        if expected.is_empty() {
            return Ok(bindings);
        }
        let consumer_binding_scope = provider_deployment_id.unwrap_or(&participant.participant_id);
        let binding_read_started = Instant::now();
        let stored = repository.get_api_bindings(consumer_binding_scope).await;
        record_duration(
            DurationMetric::AuthFlow,
            binding_read_started.elapsed(),
            "auth",
            "current_api_bindings",
            "binding_read",
            if stored.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            },
        );
        let stored = stored?;
        for api_id in expected {
            let provider_deployment_id = stored
                .get(&api_id)
                .cloned()
                .ok_or(AuthorizationStateError::NotAuthorized)?;
            bindings.insert(
                api_id,
                trellis_rs::client::AuthorizationApiBinding {
                    provider_deployment_id,
                },
            );
        }
        Ok(bindings)
    }
    .await;
    record_duration(
        DurationMetric::AuthFlow,
        total_started.elapsed(),
        "auth",
        "current_api_bindings",
        "total",
        if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        },
    );
    result
}

/// Select and persist API bindings for a participant using immutable compiled
/// evidence and compatibility results.
///
/// # Errors
///
/// Returns [`AuthorizationStateError`] when required evidence is missing or no
/// active compatible provider implements a selected API.
#[tracing::instrument(
    name = "trellis.auth.resolve_api_bindings",
    skip_all,
    fields(trellis.surface = "auth", trellis.operation = "resolve_api_bindings")
)]
pub(crate) async fn resolve_api_bindings<R>(
    repository: &R,
    participant: &ParticipantBindingRecord,
    provider_deployment_id: Option<&str>,
) -> Result<BTreeMap<String, trellis_rs::client::AuthorizationApiBinding>, AuthorizationStateError>
where
    R: DeploymentRepository + GrantRepository + Send + Sync,
{
    let total_started = Instant::now();
    let result = async {
        let consumer = repository
            .compiled_installed_evidence(&participant.evidence_digest)
            .await?;
        if consumer.package_digest != participant.package_digest {
            return Err(AuthorizationStateError::InvalidRecord(
                "compiled evidence package digest does not match the participant binding"
                    .to_owned(),
            ));
        }
        let consumer_definition =
            installed_participant(&consumer.graph, &participant.participant_id)?;
        let mut bindings = BTreeMap::new();
        for api_id in participant.projection.implemented_apis.keys() {
            bindings.insert(
                api_id.clone(),
                trellis_rs::client::AuthorizationApiBinding {
                    provider_deployment_id: provider_deployment_id
                        .ok_or(AuthorizationStateError::NotAuthorized)?
                        .to_owned(),
                },
            );
        }
        let mut selections = consumer_definition.uses().clone();
        if participant
            .projection
            .resources
            .values()
            .any(|resource| resource.kind == trellis_protocol::ParticipantResourceKind::EventConsumer)
        {
            let matching_apis = consumer
                .graph
                .packages()
                .values()
                .flat_map(|package| package.apis().keys())
                .filter(|api_id| {
                    api_id.as_str() == trellis_runtime_apis::apis::trellis_events_v1::API_ID
                })
                .cloned()
                .collect::<Vec<_>>();
            let [api_id] = matching_apis.as_slice() else {
                return Err(AuthorizationStateError::InvalidRecord(format!(
                    "event Consumer participant requires exactly one trellis.events@v1 definition, found {}",
                    matching_apis.len()
                )));
            };
            let api_id = api_id.clone();
            selections
                .entry(api_id.clone())
                .or_insert_with(|| implicit_events_selection(api_id));
        }
        if selections.is_empty() {
            return Ok(bindings);
        }
        let consumer_binding_scope = provider_deployment_id.unwrap_or(&participant.participant_id);
        let binding_read_started = Instant::now();
        let current_bindings = repository.get_api_bindings(consumer_binding_scope).await;
        record_duration(
            DurationMetric::AuthFlow,
            binding_read_started.elapsed(),
            "auth",
            "resolve_api_bindings",
            "binding_read",
            if current_bindings.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            },
        );
        let current_bindings = current_bindings?;
        let deployment_read_started = Instant::now();
        let deployments = repository.list_deployment_profiles().await;
        record_duration(
            DurationMetric::AuthFlow,
            deployment_read_started.elapsed(),
            "auth",
            "resolve_api_bindings",
            "deployment_read",
            if deployments.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            },
        );
        let mut deployments = deployments?;
        deployments.retain(|deployment| {
            deployment.state == DeploymentProfileState::Active && deployment.participant_id.is_some()
        });
        deployments.sort_by(|left, right| left.deployment_id.cmp(&right.deployment_id));
        let deployment_participants = deployments
            .iter()
            .map(|deployment| {
                (
                    deployment.deployment_id.as_str(),
                    deployment
                        .participant_id
                        .as_deref()
                        .expect("active deployments retain their participant"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut loaded_records: BTreeMap<String, Option<Arc<(u64, ParticipantBindingRecord)>>> =
            BTreeMap::new();
        let mut updates = Vec::new();
        for (api, selection) in &selections {
            let api_id = api.as_str();
            if participant.projection.implemented_apis.contains_key(api_id) {
                continue;
            }
            let mut chosen = None;
            if let Some(current) = current_bindings.get(api_id).cloned() {
                if let Some(participant_id) = deployment_participants.get(current.as_str()) {
                    if let Some(record) =
                        load_candidate_participant(repository, &mut loaded_records, participant_id)
                            .await?
                    {
                        if record.1.projection.implemented_apis.contains_key(api_id)
                            && compatible_selection(repository, &consumer, selection, &record.1)
                                .await?
                        {
                            chosen = Some(current);
                        }
                    }
                }
            }
            if chosen.is_none() {
                for deployment in &deployments {
                    if current_bindings.get(api_id) == Some(&deployment.deployment_id) {
                        continue;
                    }
                    let participant_id = deployment
                        .participant_id
                        .as_deref()
                        .expect("active deployments retain their participant");
                    let Some(record) =
                        load_candidate_participant(repository, &mut loaded_records, participant_id)
                            .await?
                    else {
                        continue;
                    };
                    if !record.1.projection.implemented_apis.contains_key(api_id) {
                        continue;
                    }
                    if compatible_selection(repository, &consumer, selection, &record.1).await? {
                        chosen = Some(deployment.deployment_id.clone());
                        break;
                    }
                }
            }
            let chosen = chosen.ok_or(AuthorizationStateError::NotAuthorized)?;
            tracing::info!(
                event = "trellis.auth.api_binding.resolve",
                participant_id = %participant.participant_id,
                binding_scope = %consumer_binding_scope,
                api_id,
                current = ?current_bindings.get(api_id),
                selected = %chosen,
                changed = current_bindings.get(api_id) != Some(&chosen),
                "resolved authorization API provider binding"
            );
            if current_bindings.get(api_id) != Some(&chosen) {
                updates.push((api_id.to_owned(), chosen.clone()));
            }
            bindings.insert(
                api_id.to_owned(),
                trellis_rs::client::AuthorizationApiBinding {
                    provider_deployment_id: chosen,
                },
            );
        }
        for (api_id, provider_deployment_id) in updates {
            repository
                .put_api_binding(consumer_binding_scope, &api_id, &provider_deployment_id)
                .await?;
        }
        Ok(bindings)
    }
    .await;
    record_duration(
        DurationMetric::AuthFlow,
        total_started.elapsed(),
        "auth",
        "resolve_api_bindings",
        "total",
        if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        },
    );
    result
}

fn installed_participant<'a>(
    graph: &'a PackageGraph,
    participant_id: &str,
) -> Result<&'a trellis_idl::ParticipantDefinition, AuthorizationStateError> {
    graph
        .root_package()
        .participants()
        .values()
        .find(|candidate| candidate.identity().as_str() == participant_id)
        .ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(
                "installed participant is absent from its package evidence".to_owned(),
            )
        })
}

async fn load_candidate_participant<R>(
    repository: &R,
    loaded: &mut BTreeMap<String, Option<Arc<(u64, ParticipantBindingRecord)>>>,
    participant_id: &str,
) -> Result<Option<Arc<(u64, ParticipantBindingRecord)>>, AuthorizationStateError>
where
    R: GrantRepository + Send + Sync,
{
    if let Some(record) = loaded.get(participant_id) {
        return Ok(record.clone());
    }
    let record = repository
        .get_installed_participant_record(participant_id.to_owned(), None)
        .await?
        .map(Arc::new);
    loaded.insert(participant_id.to_owned(), record.clone());
    Ok(record)
}

async fn compatible_selection<R>(
    repository: &R,
    consumer: &Arc<CompiledInstalledEvidence>,
    selection: &InteractionSelection,
    provider: &ParticipantBindingRecord,
) -> Result<bool, AuthorizationStateError>
where
    R: GrantRepository + Send + Sync,
{
    let provider_evidence = repository
        .compiled_installed_evidence(&provider.evidence_digest)
        .await?;
    Ok(repository
        .compare_installed_selection(Arc::clone(consumer), selection.clone(), provider_evidence)
        .await?
        .compatible)
}

fn implicit_events_selection(api_id: trellis_idl::ApiId) -> InteractionSelection {
    InteractionSelection {
        api: api_id,
        actions: [
            "Consumers.ReportDelivery",
            "Consumers.Query",
            "Consumers.Inspect",
            "DeadLetters.Query",
            "DeadLetters.Inspect",
            "DeadLetters.Replay",
            "DeadLetters.Dismiss",
        ]
        .into_iter()
        .map(|name| trellis_idl::ActionSelection {
            action: trellis_idl::ActionId {
                kind: trellis_idl::ActionKind::Rpc,
                name: name.to_owned(),
            },
            direction: trellis_idl::InteractionDirection::Call,
        })
        .collect(),
        optional_capabilities: BTreeSet::new(),
    }
}
