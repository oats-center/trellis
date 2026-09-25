use std::sync::{Arc, Mutex};

use clap::Parser;
use futures_util::stream;
use service_trellis::apis::demo_service_fieldops_v1::rpc::EvidenceDownloadOutput;
use service_trellis::participants::demo_rust_service_service::{Participant, Provider};
use service_trellis::types::{
    AssignmentsListResponse, AuditRecordedEvent, EvidenceDeleteResponse, EvidenceDownloadResponse,
    EvidenceListResponse, EvidenceRecord, EvidenceUploadProgress, EvidenceUploadResponse,
    InspectionAssignment, Priority, ReportRecord, ReportsGenerateProgress, ReportsGenerateResponse,
    ReportsListResponse, ReportsPublishedEvent, SiteSummary, SitesGetResponse, SitesListResponse,
    SitesRefreshProgress, SitesRefreshResponse, SitesRefreshedEvent,
};
use trellis_rs::jobs::JobProcessError;
use trellis_rs::service::{ServerError, ServiceConnectOptions, StoreListOptions};

const REQUEST_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Parser)]
struct Args {
    /// Print the generated contract identity and exit.
    #[arg(long)]
    contract: bool,

    /// Trellis HTTP base URL for authenticated service bootstrap.
    #[arg(long, env = "TRELLIS_URL")]
    trellis_url: Option<String>,

    /// Base64url service instance seed for authenticated bootstrap.
    #[arg(long, env = "TRELLIS_SEED")]
    seed: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    if args.contract {
        println!(
            "{} {}",
            service_trellis::participants::demo_rust_service_service::PARTICIPANT_ID,
            service_trellis::participants::demo_rust_service_service::PARTICIPANT_DIGEST,
        );
        return Ok(());
    }

    let (trellis_url, seed) = match (args.trellis_url, args.seed) {
        (Some(trellis_url), Some(seed)) => (trellis_url, seed),
        (None, None) => {
            println!("Rust Field Ops service demo: provide --trellis-url and --seed to run.");
            return Ok(());
        }
        _ => anyhow::bail!("--trellis-url and --seed must be provided together"),
    };

    let mut runtime = Participant::connect(
        ServiceConnectOptions::new(&trellis_url, &seed)
            .with_name("rust-field-ops-demo")
            .with_timeout_ms(REQUEST_TIMEOUT_MS),
    )
    .await?;

    let client = Provider::new(&mut runtime).client();
    let sites = client.site_summaries().await?;
    let uploads = client.uploads().await?;
    seed_sites(&sites).await?;

    let assignments = Arc::new(sample_assignments());
    let reports = Arc::new(Mutex::new(Vec::<ReportRecord>::new()));
    let api_client = client.demo_service_fieldops_v1();

    {
        let mut provider = Provider::new(&mut runtime);
        provider
            .register_refresh_site_summary({
                let sites = sites.clone();
                move |job| {
                    let sites = sites.clone();
                    async move {
                        let site = refresh_site(&sites, &job.payload().site_id).await?;
                        Ok::<_, JobProcessError<String>>(service_trellis::participants::demo_rust_service_service::resources::RefreshSiteSummaryResult {
                            refresh_id: format!("refresh-{}", job.context().request_id),
                            site,
                            status: "completed".into(),
                        })
                    }
                }
            })
            .await?;

        let mut api = provider.demo_service_fieldops_v1();
        api.register_assignments_list({
            let assignments = Arc::clone(&assignments);
            move |_, _| {
                let assignments = Arc::clone(&assignments);
                async move {
                    Ok(AssignmentsListResponse {
                        items: assignments.as_ref().clone(),
                        page: Default::default(),
                    })
                }
            }
        });
        api.register_sites_list({
            let sites = sites.clone();
            move |_, _| {
                let sites = sites.clone();
                async move {
                    Ok(SitesListResponse {
                        items: list_sites(&sites).await?,
                        page: Default::default(),
                    })
                }
            }
        });
        api.register_sites_get({
            let sites = sites.clone();
            move |_, input| {
                let sites = sites.clone();
                async move {
                    Ok(SitesGetResponse {
                        site: sites
                            .get(input.site_id.as_ref())
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?,
                    })
                }
            }
        });
        api.register_evidence_list({
            let uploads = uploads.clone();
            move |_, input| {
                let uploads = uploads.clone();
                async move {
                    let cursor = input.page.as_ref().and_then(|page| page.cursor.clone());
                    let limit = input
                        .page
                        .as_ref()
                        .and_then(|page| page.limit)
                        .map(|limit| limit as usize);
                    let page = uploads
                        .list_page(StoreListOptions {
                            prefix: input.prefix.map_or_else(String::new, |prefix| prefix.0),
                            cursor,
                            limit,
                        })
                        .await?;
                    Ok(EvidenceListResponse {
                        items: page
                            .entries
                            .into_iter()
                            .map(|entry| EvidenceRecord {
                                evidence_id: entry.key.clone().into(),
                                evidence_type: "binary".to_owned().into(),
                                key: entry.key.into(),
                                size: entry.size.into(),
                                uploaded_at: entry
                                    .modified_at
                                    .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
                                    .into(),
                                file_name: None,
                                content_type: None,
                            })
                            .collect(),
                        page: service_trellis::__types::CursorPageInfo {
                            next_cursor: page.next_cursor,
                        },
                    })
                }
            }
        });
        api.register_evidence_download({
            let uploads = uploads.clone();
            move |_, input| {
                let uploads = uploads.clone();
                async move {
                    let info = uploads
                        .metadata(input.key.as_ref())
                        .await?
                        .ok_or_else(|| ServerError::Nats("evidence not found".into()))?;
                    Ok(EvidenceDownloadOutput {
                        response: EvidenceDownloadResponse {
                            key: info.key.into(),
                            size: info.size.into(),
                            digest: info.digest.unwrap_or_else(|| "unknown".into()).into(),
                            updated_at: info
                                .modified_at
                                .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
                                .into(),
                            content_type: None,
                            metadata: Default::default(),
                        },
                        transfer: None,
                    })
                }
            }
        });
        api.register_evidence_delete({
            let uploads = uploads.clone();
            move |_, input| {
                let uploads = uploads.clone();
                async move {
                    let key = input.key.to_string();
                    uploads.delete(&key).await?;
                    Ok(EvidenceDeleteResponse {
                        key: key.into(),
                        deleted: true,
                    })
                }
            }
        });
        api.register_reports_list({
            let reports = Arc::clone(&reports);
            move |_, _| {
                let reports = Arc::clone(&reports);
                async move {
                    Ok(ReportsListResponse {
                        items: reports.lock().expect("reports lock").clone(),
                        page: Default::default(),
                    })
                }
            }
        });
        api.register_sites_refresh({
            let sites = sites.clone();
            let publisher = api_client.clone();
            move |_, input, operation| {
                let sites = sites.clone();
                let publisher = publisher.clone();
                async move {
                    operation.started().await?;
                    operation
                        .progress(SitesRefreshProgress {
                            stage: "refreshing".to_owned().into(),
                            message: format!("Refreshing {}", input.site_id).into(),
                        })
                        .await?;
                    let site =
                        refresh_site(&sites, input.site_id.as_ref())
                            .await
                            .map_err(|error| {
                                ServerError::Nats(match error {
                                    JobProcessError::Retryable(error)
                                    | JobProcessError::Failed(error) => error,
                                })
                            })?;
                    let output = SitesRefreshResponse {
                        refresh_id: format!("refresh-{}", input.site_id).into(),
                        site,
                        status: "completed".to_owned().into(),
                    };
                    operation.complete(output.clone()).await?;
                    publisher
                        .publish_sites_refreshed(&SitesRefreshedEvent {
                            refresh_id: output.refresh_id,
                            site: output.site,
                            refreshed_at: "2026-05-02T00:00:00.000Z".to_owned().into(),
                        })
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    Ok(())
                }
            }
        });
        api.register_reports_generate({
            let reports = Arc::clone(&reports);
            let assignments = Arc::clone(&assignments);
            let publisher = api_client.clone();
            move |_, input, operation| {
                let reports = Arc::clone(&reports);
                let assignments = Arc::clone(&assignments);
                let publisher = publisher.clone();
                async move {
                    operation.started().await?;
                    operation
                        .progress(ReportsGenerateProgress {
                            stage: "generating".to_owned().into(),
                            message: "Generating closeout report".to_owned().into(),
                        })
                        .await?;
                    let assignment = assignments
                        .iter()
                        .find(|assignment| assignment.inspection_id == input.inspection_id)
                        .cloned();
                    let report_id = format!("closeout-{}", input.inspection_id);
                    reports.lock().expect("reports lock").push(ReportRecord {
                        report_id: report_id.clone().into(),
                        inspection_id: input.inspection_id.clone(),
                        site_id: assignment.as_ref().map(|value| value.site_id.clone()),
                        site_name: assignment.as_ref().map_or_else(
                            || "Unknown site".to_owned().into(),
                            |value| value.site_name.clone(),
                        ),
                        asset_name: assignment.as_ref().map_or_else(
                            || "Unknown asset".to_owned().into(),
                            |value| value.asset_name.clone(),
                        ),
                        status: "published".to_owned().into(),
                        published_at: "2026-05-02T00:00:00.000Z".to_owned().into(),
                        report_comment: input.report_comment.clone(),
                        summary: "Generated closeout report".to_owned().into(),
                        readiness: "Site context reconciled".to_owned().into(),
                        evidence_status: "Evidence review completed".to_owned().into(),
                    });
                    let output = ReportsGenerateResponse {
                        report_id: report_id.clone().into(),
                        inspection_id: input.inspection_id.clone(),
                        status: "published".to_owned().into(),
                    };
                    operation.complete(output).await?;
                    publisher
                        .publish_reports_published(&ReportsPublishedEvent {
                            report_id: report_id.into(),
                            inspection_id: input.inspection_id,
                            site_id: assignment.map(|value| value.site_id),
                            published_at: "2026-05-02T00:00:00.000Z".to_owned().into(),
                        })
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    Ok(())
                }
            }
        });
        api.register_evidence_upload({
            let publisher = api_client.clone();
            move |_, input, operation| {
                let publisher = publisher.clone();
                async move {
                    operation.started().await?;
                    operation
                        .progress(EvidenceUploadProgress {
                            stage: "receiving".to_owned().into(),
                            message: "Receiving evidence".to_owned().into(),
                        })
                        .await?;
                    let info = operation
                        .upload()
                        .await?
                        .ok_or_else(|| ServerError::Nats("upload transfer is required".into()))?;
                    let evidence_id = format!("evidence-{}", info.key);
                    operation
                        .complete(EvidenceUploadResponse {
                            evidence_id: evidence_id.clone().into(),
                            key: info.key.clone().into(),
                            size: info.size.into(),
                            disposition: "stored".to_owned().into(),
                            file_name: None,
                            content_type: input.content_type,
                        })
                        .await?;
                    publisher
                        .publish_audit_recorded(&AuditRecordedEvent {
                            activity_id: format!("activity-{evidence_id}").into(),
                            kind: "evidence-uploaded".to_owned().into(),
                            message: format!("Uploaded {}", info.key).into(),
                            occurred_at: "2026-05-02T00:00:00.000Z".to_owned().into(),
                            related_site_id: None,
                            related_inspection_id: None,
                        })
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    Ok(())
                }
            }
        });
        api.register_audit_feed(|_, _| stream::empty());
    }

    tracing::info!("starting Rust demo service request loop");
    runtime.run().await?;
    Ok(())
}

async fn seed_sites(sites: &trellis_rs::service::KvHandle<SiteSummary>) -> Result<(), ServerError> {
    for site in sample_sites() {
        if sites
            .get(site.site_id.as_ref())
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?
            .is_none()
        {
            sites
                .create(site.site_id.as_ref(), &site)
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
        }
    }
    Ok(())
}

async fn list_sites(
    sites: &trellis_rs::service::KvHandle<SiteSummary>,
) -> Result<Vec<SiteSummary>, ServerError> {
    let mut values = Vec::new();
    for site in sample_sites() {
        if let Some(value) = sites
            .get(site.site_id.as_ref())
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?
        {
            values.push(value);
        }
    }
    Ok(values)
}

async fn refresh_site(
    sites: &trellis_rs::service::KvHandle<SiteSummary>,
    site_id: &str,
) -> Result<SiteSummary, JobProcessError<String>> {
    let mut site = sites
        .get_entry(site_id)
        .await
        .map_err(|error| JobProcessError::retryable(error.to_string()))?
        .ok_or_else(|| JobProcessError::failed(format!("unknown site '{site_id}'")))?;
    let mut value = site
        .value
        .take()
        .ok_or_else(|| JobProcessError::failed(format!("site '{site_id}' was deleted")))?;
    value.last_report_at = "2026-05-02T00:00:00.000Z".to_owned().into();
    sites
        .replace(site_id, site.revision, &value)
        .await
        .map_err(|error| JobProcessError::retryable(error.to_string()))?;
    Ok(value)
}

fn sample_sites() -> Vec<SiteSummary> {
    vec![
        SiteSummary {
            site_id: "site-north".to_owned().into(),
            site_name: "North Ridge Substation".to_owned().into(),
            open_inspections: 3,
            overdue_inspections: 1,
            latest_status: "attention".to_owned().into(),
            last_report_at: "2026-04-30T16:00:00.000Z".to_owned().into(),
        },
        SiteSummary {
            site_id: "site-south".to_owned().into(),
            site_name: "South Canal Pump Station".to_owned().into(),
            open_inspections: 2,
            overdue_inspections: 0,
            latest_status: "healthy".to_owned().into(),
            last_report_at: "2026-05-01T09:30:00.000Z".to_owned().into(),
        },
    ]
}

fn sample_assignments() -> Vec<InspectionAssignment> {
    vec![InspectionAssignment {
        inspection_id: "inspection-1001".to_owned().into(),
        site_id: "site-north".to_owned().into(),
        site_name: "North Ridge Substation".to_owned().into(),
        asset_name: "Transformer T-17".to_owned().into(),
        checklist_name: "Quarterly transformer inspection".to_owned().into(),
        scheduled_for: "2026-05-03T08:00:00.000Z".to_owned().into(),
        priority: Priority::High,
    }]
}
