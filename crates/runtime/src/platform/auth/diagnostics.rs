//! `Auth.Runtime.Diagnostics`: one report of the installation's state, so an
//! operator with an admin login can diagnose a deployment without a shell on
//! its host.
//!
//! The platform answers because it holds most of what the report needs: the
//! Auth database (deployments, instances and what they wait for, enforcement,
//! set-aside effects), the listener's certificate, and the failures of the
//! requests it serves. Health comes from this host's Health database, read
//! only, as `Health.Storage` reads every database; JetStream comes from the
//! same capacity read `Health.Storage` uses. Each section reports what it
//! could not read as a finding instead of failing the report.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use time::OffsetDateTime;
use trellis_runtime_apis::__types::CursorQuery;
use trellis_runtime_apis::apis::trellis_auth_v1::Provider;
use trellis_runtime_apis::types::{
    AuthInstanceState, AuthInstancesListRequest, AuthRuntimeDiagnosticsResponse,
    HealthInspectRequest, HealthInspectRequestContractId, HealthInspectRequestHistoryLimit,
    HealthInspectRequestParticipantKind, HealthQueryRequest, HealthQueryRequestParticipantKinds,
    HealthQueryRequestStatuses,
};

use super::rpc::{caller, rpc_error};
use super::{blocking, AuthError, OAuthEngine};
use crate::capacity::{Capacity, Shortages};
use crate::health::HealthStore;
use crate::{ListenerTransport, RuntimeConfig, RuntimeMode, StorageBackend};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// A certificate this close to expiring is a warning.
const CERTIFICATE_WARNING_DAYS: i64 = 30;
/// How many failures and set-aside records the report lists.
const LISTED: usize = 20;

/// What the diagnostics report reads beyond the Auth store.
#[derive(Clone)]
pub(crate) struct Diagnostics {
    pub(crate) config: Arc<RuntimeConfig>,
    pub(crate) mode: RuntimeMode,
    pub(crate) nats: async_nats::Client,
    pub(crate) shortages: Arc<Shortages>,
    pub(crate) started_at: OffsetDateTime,
}

impl std::fmt::Debug for Diagnostics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Diagnostics")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// How serious a finding is; a section is as serious as its worst finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Status {
    Ok,
    Warning,
    Problem,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Problem => "problem",
        }
    }
}

type Findings = Vec<(Status, String)>;

/// `fields` with the section's `status` and `findings` added.
fn section(findings: &[(Status, String)], mut fields: Value) -> (Status, Value) {
    let status = findings
        .iter()
        .map(|(status, _)| *status)
        .max()
        .unwrap_or(Status::Ok);
    fields["status"] = json!(status.as_str());
    fields["findings"] = findings
        .iter()
        .map(|(status, message)| json!({ "status": status.as_str(), "message": message }))
        .collect();
    (status, fields)
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn from_seconds(seconds: i64) -> String {
    OffsetDateTime::from_unix_timestamp(seconds)
        .map(rfc3339)
        .unwrap_or_default()
}

/// Serve `Runtime.Diagnostics` on the platform provider.
pub(crate) fn register_rpc<P: trellis_rs::generated::ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    oauth: Arc<OAuthEngine>,
    diagnostics: Diagnostics,
) {
    provider.register_runtime_diagnostics(move |context, _input| {
        let oauth = Arc::clone(&oauth);
        let diagnostics = diagnostics.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            report(oauth, diagnostics).await.map_err(rpc_error)
        }
    });
}

async fn report(
    oauth: Arc<OAuthEngine>,
    diagnostics: Diagnostics,
) -> Result<AuthRuntimeDiagnosticsResponse, AuthError> {
    let now = OffsetDateTime::now_utc();
    let config = Arc::clone(&diagnostics.config);
    let stored =
        blocking(move || Ok::<_, AuthError>(stored_sections(&oauth, &config, now))).await?;
    let jetstream = jetstream(&diagnostics).await;
    let server = section(
        &[],
        json!({
            "version": VERSION,
            "installationId": diagnostics
                .config
                .auth
                .as_ref()
                .and_then(|auth| auth.instance_id.clone())
                .unwrap_or_default(),
            "mode": diagnostics.mode.to_string(),
            "startedAt": rfc3339(diagnostics.started_at),
            "uptimeSeconds": (now - diagnostics.started_at).whole_seconds().max(0).to_string(),
        }),
    );
    let certificates = certificates(&diagnostics.config, now);
    let failures = failures(now);
    let sections = [
        ("server", server),
        ("health", stored.health),
        ("jetstream", jetstream),
        ("disk", stored.disk),
        ("certificates", certificates),
        ("failures", failures),
        ("work", stored.work),
    ];
    let mut response = json!({ "asOf": rfc3339(now) });
    let mut worst = Status::Ok;
    for (name, (status, value)) in sections {
        worst = worst.max(status);
        response[name] = value;
    }
    response["status"] = json!(worst.as_str());
    serde_json::from_value(response).map_err(|error| AuthError::Invalid(error.to_string()))
}

/// The sections read from the databases, on the blocking pool.
struct Stored {
    health: (Status, Value),
    disk: (Status, Value),
    work: (Status, Value),
}

fn stored_sections(oauth: &OAuthEngine, config: &RuntimeConfig, now: OffsetDateTime) -> Stored {
    let health_store = crate::health::open_reader(config);
    Stored {
        health: health(oauth, health_store.as_ref(), now),
        disk: disk(config),
        work: work(oauth, health_store.as_ref()),
    }
}

/// Each active service deployment and its active instances: what Auth knows
/// of each (connected, waiting for settings or approval) joined with the
/// health it reports on its current connection, as `svc <id> instances` shows
/// it. Devices are listed only when they report degraded or unhealthy.
fn health(
    oauth: &OAuthEngine,
    store: Option<&HealthStore>,
    now: OffsetDateTime,
) -> (Status, Value) {
    let mut findings = Findings::new();
    let storage = oauth.server.store();
    let read = storage.authority.read(|connection| {
        let deployments = connection
            .prepare(
                "SELECT deployment_id,participant_id,title FROM auth_deployments \
                WHERE participant_kind='service' AND state='active' ORDER BY title,deployment_id",
            )?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut instances = Vec::new();
        let mut cursor = None;
        loop {
            let request: AuthInstancesListRequest = serde_json::from_value(json!({
                "page": { "cursor": cursor, "limit": 200 },
            }))?;
            let page = super::rpc_deployments::read_instances(connection, &request, "service")?;
            for mut instance in page.items {
                if instance.state != AuthInstanceState::Active || !instance.principal.enabled {
                    continue;
                }
                if instance.connected_at.is_some() {
                    instance.waiting =
                        super::rpc_deployments::waiting(connection, storage, &instance)?;
                }
                instances.push(serde_json::to_value(instance)?);
            }
            match page.page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok((deployments, instances))
    });
    let (deployments, instances) = match read {
        Ok(read) => read,
        Err(error) => {
            findings.push((Status::Problem, format!("cannot read deployments: {error}")));
            return section(&findings, json!({ "deployments": [] }));
        }
    };
    if store.is_none() {
        findings.push((
            Status::Warning,
            "the Health database is not on this host, so instance health is not shown".into(),
        ));
    }
    let now_ns = i64::try_from(now.unix_timestamp_nanos()).unwrap_or(i64::MAX);
    let mut listed = Vec::new();
    for (deployment_id, participant_id, title) in deployments {
        let reports = store
            .and_then(|store| {
                let request = HealthInspectRequest {
                    contract_id: HealthInspectRequestContractId(participant_id.clone()),
                    history_limit: Some(HealthInspectRequestHistoryLimit(1.into())),
                    history_since: None,
                    instance_id: None,
                    participant_kind: HealthInspectRequestParticipantKind::Service,
                    extra: Default::default(),
                };
                store.inspect(&request, now_ns).ok().flatten()
            })
            .and_then(|inspected| serde_json::to_value(inspected).ok());
        let reports = reports
            .as_ref()
            .and_then(|reports| reports["instances"].as_array())
            .map_or(&[][..], Vec::as_slice);
        let mut deployment_status = Status::Ok;
        let mut connected = 0;
        let mut rows = Vec::new();
        for instance in instances
            .iter()
            .filter(|instance| instance["deploymentId"] == deployment_id.as_str())
        {
            let id = instance["instanceId"].as_str().unwrap_or_default();
            let name = format!("{title}: instance {id}");
            let (state, status) = instance_state(instance, reports, &name, &mut findings);
            if state != "offline" {
                connected += 1;
            }
            deployment_status = deployment_status.max(status);
            rows.push(json!({ "instanceId": id, "state": state, "status": status.as_str() }));
        }
        if connected == 0 {
            findings.push((
                Status::Warning,
                format!("{title}: no instance is connected"),
            ));
            deployment_status = deployment_status.max(Status::Warning);
        }
        listed.push(json!({
            "deploymentId": deployment_id,
            "participantId": participant_id,
            "title": title,
            "status": deployment_status.as_str(),
            "instances": rows,
        }));
    }
    if let Some(store) = store {
        let request = HealthQueryRequest {
            contract_ids: None,
            deployment_ids: None,
            page: Some(CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            participant_kinds: Some(vec![HealthQueryRequestParticipantKinds::Device]),
            search: None,
            statuses: Some(vec![
                HealthQueryRequestStatuses::Degraded,
                HealthQueryRequestStatuses::Unhealthy,
            ]),
            extra: Default::default(),
        };
        if let Some(devices) = store
            .query(&request, now_ns)
            .ok()
            .and_then(|devices| serde_json::to_value(devices).ok())
        {
            for device in devices["items"].as_array().map_or(&[][..], Vec::as_slice) {
                let status = device["effectiveStatus"].as_str().unwrap_or_default();
                if status != "degraded" && status != "unhealthy" {
                    continue;
                }
                findings.push((
                    if status == "unhealthy" {
                        Status::Problem
                    } else {
                        Status::Warning
                    },
                    format!(
                        "device {} ({}) is {status}",
                        device["participantName"].as_str().unwrap_or_default(),
                        device["contractId"].as_str().unwrap_or_default()
                    ),
                ));
            }
        }
    }
    section(&findings, json!({ "deployments": listed }))
}

/// One instance's state and status, adding a finding for each thing wrong.
///
/// A report from an earlier connection says nothing about the current one,
/// so health counts only once the instance reports on this connection.
fn instance_state(
    instance: &Value,
    reports: &[Value],
    name: &str,
    findings: &mut Findings,
) -> (&'static str, Status) {
    let parse = |value: &Value| {
        value.as_str().and_then(|raw| {
            OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
        })
    };
    let Some(connected_at) = parse(&instance["connectedAt"]) else {
        return ("offline", Status::Ok);
    };
    let waiting = &instance["waiting"];
    let missing: Vec<&str> = waiting["missingSettings"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !missing.is_empty() {
        let mut message = format!("{name} waits for settings: {}", missing.join(", "));
        if let Some(url) = waiting["settingsUrl"].as_str() {
            message.push_str(&format!("; set them at {url}"));
        }
        findings.push((Status::Problem, message));
        return ("waitingForSettings", Status::Problem);
    }
    let report = reports
        .iter()
        .find(|report| report["instanceId"] == instance["instanceId"])
        .filter(|report| parse(&report["observedAt"]).is_some_and(|at| at >= connected_at));
    let Some(report) = report else {
        if !waiting["update"].is_null() {
            let mut message = format!("{name} waits for an administrator to approve its update");
            if let Some(url) = waiting["update"]["reviewUrl"].as_str() {
                message.push_str(&format!(" at {url}"));
            }
            findings.push((Status::Warning, message));
            return ("waitingForApproval", Status::Warning);
        }
        return ("starting", Status::Ok);
    };
    let (state, mut status) = match report["effectiveStatus"].as_str() {
        Some("healthy") => ("healthy", Status::Ok),
        Some("degraded") => ("degraded", Status::Warning),
        Some("unhealthy") => ("unhealthy", Status::Problem),
        _ => {
            findings.push((
                Status::Problem,
                format!(
                    "{name} is connected but its heartbeats stopped; the last was at {}",
                    report["latestSample"]["sample"]["time"]
                        .as_str()
                        .unwrap_or("an unknown time")
                ),
            ));
            return ("unresponsive", Status::Problem);
        }
    };
    let checks = report["latestSample"]["checks"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    for check in checks {
        let check_status = check["status"].as_str().unwrap_or_default();
        if check_status == "ok" {
            continue;
        }
        let severity = if check_status == "degraded" {
            Status::Warning
        } else {
            Status::Problem
        };
        status = status.max(severity);
        let detail = check["summary"]
            .as_str()
            .or_else(|| check["error"].as_str())
            .unwrap_or("no message");
        findings.push((
            severity,
            format!(
                "{name}: check {} {check_status}: {detail}",
                check["name"].as_str().unwrap_or_default()
            ),
        ));
    }
    if status != Status::Ok && checks.iter().all(|check| check["status"] == "ok") {
        findings.push((status, format!("{name} reports {state}")));
    }
    (state, status)
}

/// JetStream usage against its limits, from the read `Health.Storage` makes:
/// a limit 85% committed is a warning, and a refusal for lack of room a
/// problem.
async fn jetstream(diagnostics: &Diagnostics) -> (Status, Value) {
    let mut findings = Findings::new();
    let usage = |usage: &crate::capacity::Usage| {
        json!({
            "used": usage.used.to_string(),
            "reserved": usage.reserved.to_string(),
            "limit": usage.limit.map(|limit| limit.to_string()),
        })
    };
    let fields = match Capacity::read(&diagnostics.nats, diagnostics.shortages.system()).await {
        Ok(capacity) => {
            findings.extend(
                capacity
                    .pressure()
                    .into_iter()
                    .map(|problem| (Status::Warning, problem)),
            );
            json!({
                "storage": usage(&capacity.storage),
                "memory": usage(&capacity.memory),
                "serverStorage": capacity.server_storage.as_ref().map(usage),
                "serverMemory": capacity.server_memory.as_ref().map(usage),
                "streams": capacity.streams.to_string(),
                "maxStreams": capacity.max_streams.map(|max| max.to_string()),
                "consumers": capacity.consumers.to_string(),
                "maxConsumers": capacity.max_consumers.map(|max| max.to_string()),
            })
        }
        Err(error) => {
            findings.push((
                Status::Problem,
                format!("JetStream usage could not be read: {error}"),
            ));
            json!({})
        }
    };
    findings.extend(
        diagnostics
            .shortages
            .outstanding()
            .into_iter()
            .map(|refusal| (Status::Problem, refusal)),
    );
    section(&findings, fields)
}

/// Free and total space of the file system holding the Auth database: below
/// 10% free is a warning, below 5% a problem.
fn disk(config: &RuntimeConfig) -> (Status, Value) {
    let mut findings = Findings::new();
    let path = match config.platform_storage_backend() {
        Ok(StorageBackend::Sqlite(sqlite)) => sqlite
            .path
            .parent()
            .map_or_else(|| sqlite.path.clone(), Path::to_path_buf),
        _ => PathBuf::new(),
    };
    let space = file_system_space(&path);
    match space {
        Some((free, total)) if total > 0 => {
            let fraction = free as f64 / total as f64;
            let status = if fraction < 0.05 {
                Status::Problem
            } else if fraction < 0.10 {
                Status::Warning
            } else {
                Status::Ok
            };
            if status != Status::Ok {
                findings.push((
                    status,
                    format!(
                        "{} has {} free of {} ({:.0}%)",
                        path.display(),
                        crate::capacity::bytes(free),
                        crate::capacity::bytes(total),
                        fraction * 100.0
                    ),
                ));
            }
        }
        _ => findings.push((
            Status::Warning,
            format!("the free space of {} could not be read", path.display()),
        )),
    }
    section(
        &findings,
        json!({
            "path": path.display().to_string(),
            "free": space.map(|(free, _)| free.to_string()),
            "total": space.map(|(_, total)| total.to_string()),
        }),
    )
}

/// Bytes available to the server and the file system's size.
#[cfg(unix)]
// The `statvfs` field types differ between platforms; on some they are `u64`.
#[allow(clippy::unnecessary_cast)]
fn file_system_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a NUL-terminated string that outlives the call, and
    // `stat` points to writable memory for one `statvfs`.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statvfs` returned 0, so it filled in `stat`.
    let stat = unsafe { stat.assume_init() };
    let block = (stat.f_frsize as u64).max(1);
    Some((
        (stat.f_bavail as u64).saturating_mul(block),
        (stat.f_blocks as u64).saturating_mul(block),
    ))
}

#[cfg(not(unix))]
fn file_system_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// The certificate the listener serves and the CA that issued a generated
/// one: expired is a problem, expiring within 30 days a warning.
fn certificates(config: &RuntimeConfig, now: OffsetDateTime) -> (Status, Value) {
    let mut findings = Findings::new();
    let transport = config
        .http
        .as_ref()
        .and_then(|http| http.transport.as_ref());
    let (transport, served) = match transport {
        Some(ListenerTransport::Tls {
            certificate_chain, ..
        }) => ("tls", Some(certificate_chain.clone())),
        Some(ListenerTransport::Proxy { .. }) => ("proxy", None),
        Some(ListenerTransport::Generated) => ("tls", None),
        Some(ListenerTransport::LoopbackDevelopment) | None => ("loopbackDevelopment", None),
    };
    let generated = config
        .tls
        .as_ref()
        .and_then(|tls| tls.generated_ca_certificate.clone());
    let mut listed = Vec::new();
    for (role, path) in [("served", served), ("issuingCa", generated)] {
        let Some(path) = path else { continue };
        let what = if role == "served" {
            "the served certificate"
        } else {
            "the issuing CA"
        };
        match crate::tls::inspect_certificate(&path) {
            Ok(summary) => {
                let left = summary.not_after - now;
                if left.is_negative() {
                    findings.push((
                        Status::Problem,
                        format!(
                            "{what} {} expired at {}",
                            summary.subject,
                            rfc3339(summary.not_after)
                        ),
                    ));
                } else if left.whole_days() < CERTIFICATE_WARNING_DAYS {
                    findings.push((
                        Status::Warning,
                        format!(
                            "{what} {} expires in {} days, at {}",
                            summary.subject,
                            left.whole_days(),
                            rfc3339(summary.not_after)
                        ),
                    ));
                }
                listed.push(json!({
                    "role": role,
                    "path": path.display().to_string(),
                    "subject": summary.subject,
                    "issuer": summary.issuer,
                    "notAfter": rfc3339(summary.not_after),
                    "sha256Fingerprint": summary.sha256_fingerprint,
                }));
            }
            Err(error) => {
                findings.push((Status::Problem, format!("{what} cannot be read: {error}")))
            }
        }
    }
    section(
        &findings,
        json!({ "transport": transport, "certificates": listed }),
    )
}

/// The failures this process logged with a reference, newest first. One in
/// the last hour is a warning: something failed that a caller saw.
fn failures(now: OffsetDateTime) -> (Status, Value) {
    let recent = crate::failures::recent();
    let mut findings = Findings::new();
    let last_hour = recent
        .iter()
        .filter(|failure| now - failure.at < time::Duration::HOUR)
        .count();
    if let Some(newest) = recent.first().filter(|_| last_hour > 0) {
        findings.push((
            Status::Warning,
            format!(
                "{last_hour} server failure{} in the last hour; the newest is {} ({}): {}",
                if last_hour == 1 { "" } else { "s" },
                newest.id,
                newest.route.as_deref().unwrap_or("no route"),
                newest.error
            ),
        ));
    }
    section(
        &findings,
        json!({
            "failures": recent.iter().take(LISTED).map(|failure| json!({
                "id": failure.id,
                "at": rfc3339(failure.at),
                "route": failure.route,
                "code": failure.code,
                "error": failure.error,
            })).collect::<Vec<_>>(),
        }),
    )
}

/// Enforcement whose pages keep failing is a problem: sessions it should end
/// stay usable. Effects and health status events set aside are a warning:
/// they never happened, and are kept for inspection.
fn work(oauth: &OAuthEngine, health: Option<&HealthStore>) -> (Status, Value) {
    let mut findings = Findings::new();
    let authority = &oauth.server.store().authority;
    let stuck = authority.stuck_enforcement(LISTED).unwrap_or_else(|error| {
        findings.push((
            Status::Problem,
            format!("cannot read enforcement work: {error}"),
        ));
        Vec::new()
    });
    for (work, since, error) in &stuck {
        findings.push((
            Status::Problem,
            format!(
                "enforcement work {work} has failed since {}: {error}",
                from_seconds(*since)
            ),
        ));
    }
    let mut set_aside: Vec<(String, String, String)> = authority
        .set_aside_effects(LISTED)
        .unwrap_or_else(|error| {
            findings.push((
                Status::Warning,
                format!("cannot read set-aside effects: {error}"),
            ));
            Vec::new()
        })
        .into_iter()
        .map(|(kind, at, error)| (kind, from_seconds(at), error))
        .collect();
    if !set_aside.is_empty() {
        findings.push((
            Status::Warning,
            format!(
                "{} Auth effect{} set aside because trying again cannot help; the newest: {} at {}: {}",
                set_aside.len(),
                if set_aside.len() == 1 { "" } else { "s" },
                set_aside[0].0,
                set_aside[0].1,
                set_aside[0].2
            ),
        ));
    }
    let health_set_aside = health
        .and_then(|store| store.set_aside_transitions(LISTED).ok())
        .unwrap_or_default();
    if let Some((at, error)) = health_set_aside.first() {
        findings.push((
            Status::Warning,
            format!(
                "{} health status event{} set aside; the newest at {}: {error}",
                health_set_aside.len(),
                if health_set_aside.len() == 1 { "" } else { "s" },
                OffsetDateTime::from_unix_timestamp_nanos(i128::from(*at))
                    .map(rfc3339)
                    .unwrap_or_default()
            ),
        ));
    }
    set_aside.extend(health_set_aside.into_iter().map(|(at, error)| {
        (
            "healthStatusEvent".to_owned(),
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(at))
                .map(rfc3339)
                .unwrap_or_default(),
            error,
        )
    }));
    section(
        &findings,
        json!({
            "stuckEnforcement": stuck.iter().map(|(work, since, error)| json!({
                "workId": work,
                "since": from_seconds(*since),
                "error": error,
            })).collect::<Vec<_>>(),
            "setAside": set_aside.iter().map(|(kind, at, error)| json!({
                "kind": kind,
                "at": at,
                "error": error,
            })).collect::<Vec<_>>(),
        }),
    )
}
