use std::collections::{BTreeMap, BTreeSet};

use rusqlite::params;
use serde::{de::DeserializeOwned, Serialize};
use trellis_protocol::{
    decode_pagination_cursor, encode_pagination_cursor, pagination_query_digest,
};
use trellis_runtime_apis::__types::trellis::HealthHeartbeatSamplechecksItem as HealthHeartbeatSampleChecksItem;
use trellis_runtime_apis::__types::{CursorPageInfo, CursorQuery};
use trellis_runtime_apis::types::{
    HealthInspectRequest, HealthInspectResponse,
    HealthInspectResponsehistoryItem as HealthInspectResponseHistoryItem,
    HealthInspectResponsehistoryItemchecksItem as HealthInspectResponseHistoryItemChecksItem,
    HealthInspectResponseinstancesItem as HealthInspectResponseInstancesItem,
    HealthInspectResponseinstancesItemlatestSample as HealthInspectResponseInstancesItemLatestSample,
    HealthInspectResponseparticipant as HealthInspectResponseParticipant,
    HealthInspectResponseprojection as HealthInspectResponseProjection, HealthMetricsRequest,
    HealthMetricsResponse, HealthMetricsResponseprojection as HealthMetricsResponseProjection,
    HealthMetricsResponseseriesItem as HealthMetricsResponseSeriesItem,
    HealthMetricsResponseseriesItembucketsItem as HealthMetricsResponseSeriesItemBucketsItem,
    HealthMetricsResponseseriesItembucketsItemchecksItem as HealthMetricsResponseSeriesItemBucketsItemChecksItem,
    HealthMetricsResponsesummary as HealthMetricsResponseSummary, HealthQueryRequest,
    HealthQueryResponse, HealthQueryResponseentriesItem as HealthQueryResponseEntriesItem,
    HealthSummaryRequest, HealthSummaryResponse, Number, Uint64,
};

use super::store::{rfc3339, HealthStore, HealthStoreError};

#[derive(Debug)]
struct LatestRow {
    participant_kind: String,
    contract_id: String,
    instance_id: String,
    deployment_id: String,
    participant_name: String,
    contract_digest: String,
    reported_status: String,
    effective_status: String,
    observed_at_ns: i64,
    heartbeat_deadline_ns: i64,
    started_at: String,
    runtime: String,
    version: Option<String>,
    latest_sample_json: String,
}

#[derive(Debug)]
struct ParticipantGroup {
    participant_kind: String,
    contract_id: String,
    participant_name: String,
    participant_name_observed_at: i64,
    online_instances: i64,
    offline_instances: i64,
    deployment_ids: BTreeSet<String>,
    contract_digests: BTreeSet<String>,
    has_degraded: bool,
    has_unhealthy: bool,
    last_seen_at_ns: i64,
    versions: BTreeSet<String>,
    runtimes: BTreeSet<String>,
}

impl ParticipantGroup {
    fn effective_status(&self) -> &'static str {
        if self.online_instances == 0 {
            "offline"
        } else if self.has_unhealthy {
            "unhealthy"
        } else if self.has_degraded || self.offline_instances > 0 {
            "degraded"
        } else {
            "healthy"
        }
    }
}

impl HealthStore {
    pub(crate) fn query(
        &self,
        request: &HealthQueryRequest,
        now_ns: i64,
    ) -> Result<HealthQueryResponse, HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let rows = load_latest(&connection)?;
        let mut groups = BTreeMap::<(String, String), ParticipantGroup>::new();

        // Filter in memory until participant cardinality proves SQL composition necessary.
        for row in rows {
            if !matches_filter(request.participant_kinds.as_ref(), &row.participant_kind)
                || !matches_filter(request.contract_ids.as_ref(), &row.contract_id)
                || !matches_filter(request.deployment_ids.as_ref(), &row.deployment_id)
            {
                continue;
            }
            let effective_status = if row.heartbeat_deadline_ns <= now_ns {
                "offline"
            } else {
                row.effective_status.as_str()
            };
            let group = groups
                .entry((row.participant_kind.clone(), row.contract_id.clone()))
                .or_insert_with(|| ParticipantGroup {
                    participant_kind: row.participant_kind.clone(),
                    contract_id: row.contract_id.clone(),
                    participant_name: row.participant_name.clone(),
                    participant_name_observed_at: row.observed_at_ns,
                    online_instances: 0,
                    offline_instances: 0,
                    deployment_ids: BTreeSet::new(),
                    contract_digests: BTreeSet::new(),
                    has_degraded: false,
                    has_unhealthy: false,
                    last_seen_at_ns: row.observed_at_ns,
                    versions: BTreeSet::new(),
                    runtimes: BTreeSet::new(),
                });
            if row.observed_at_ns > group.participant_name_observed_at {
                group.participant_name = row.participant_name;
                group.participant_name_observed_at = row.observed_at_ns;
            }
            group.last_seen_at_ns = group.last_seen_at_ns.max(row.observed_at_ns);
            group.deployment_ids.insert(row.deployment_id);
            group.contract_digests.insert(row.contract_digest);
            if effective_status == "offline" {
                group.offline_instances += 1;
            } else {
                group.online_instances += 1;
                group.has_degraded |= effective_status == "degraded";
                group.has_unhealthy |= effective_status == "unhealthy";
            }
            if let Some(version) = row.version {
                group.versions.insert(version);
            }
            group.runtimes.insert(row.runtime);
        }

        let search = request
            .search
            .as_ref()
            .map(|value| value.as_ref().to_lowercase());
        let mut entries = groups
            .into_values()
            .filter(|group| {
                matches_filter(request.statuses.as_ref(), group.effective_status())
                    && search.as_ref().is_none_or(|search| {
                        group.participant_name.to_lowercase().contains(search)
                            || group.contract_id.to_lowercase().contains(search)
                    })
            })
            .map(|group| {
                let effective_status = group.effective_status().to_string();
                Ok(HealthQueryResponseEntriesItem {
                    participant_kind: wire(group.participant_kind)?,
                    contract_id: group.contract_id,
                    participant_name: group.participant_name,
                    effective_status: wire(effective_status)?,
                    deployment_ids: group.deployment_ids.into_iter().map(Into::into).collect(),
                    contract_digests: group.contract_digests.into_iter().map(Into::into).collect(),
                    online_instances: Uint64(group.online_instances.try_into().unwrap_or_default()),
                    offline_instances: Uint64(
                        group.offline_instances.try_into().unwrap_or_default(),
                    ),
                    last_seen_at: rfc3339(group.last_seen_at_ns)?,
                    versions: group.versions.into_iter().collect(),
                    runtimes: group.runtimes.into_iter().collect(),
                })
            })
            .collect::<Result<Vec<_>, HealthStoreError>>()?;
        entries.sort_by(|left, right| {
            left.participant_kind
                .as_str()
                .cmp(right.participant_kind.as_str())
                .then_with(|| left.contract_id.cmp(&right.contract_id))
        });
        let query_digest = query_digest(request)?;
        let after = request
            .page
            .as_ref()
            .and_then(|page| page.cursor.as_deref())
            .map(|cursor| decode_cursor(cursor, &query_digest))
            .transpose()?;
        if let Some(after) = after {
            entries.retain(|entry| {
                (entry.participant_kind.as_str(), entry.contract_id.as_str())
                    > (after.0.as_str(), after.1.as_str())
            });
        }
        let limit = request
            .page
            .as_ref()
            .and_then(|page| page.limit)
            .unwrap_or(50);
        if limit == 0 || limit > 200 {
            return Err(HealthStoreError::InvalidPagination);
        }
        let has_more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        let next_cursor = has_more
            .then(|| entries.last())
            .flatten()
            .map(|entry| encode_cursor(&query_digest, entry))
            .transpose()?;
        Ok(HealthQueryResponse {
            items: entries,
            page: CursorPageInfo { next_cursor },
        })
    }

    pub(crate) fn summary(
        &self,
        request: &HealthSummaryRequest,
        now_ns: i64,
    ) -> Result<HealthSummaryResponse, HealthStoreError> {
        let mut query_request: HealthQueryRequest =
            serde_json::from_value(serde_json::to_value(request)?)?;
        let mut count = 0usize;
        loop {
            query_request.page = Some(CursorQuery {
                cursor: query_request.page.and_then(|page| page.cursor),
                limit: Some(200),
            });
            let page = self.query(&query_request, now_ns)?;
            count += page.items.len();
            let Some(cursor) = page.page.next_cursor else {
                break;
            };
            query_request.page = Some(CursorQuery {
                cursor: Some(cursor),
                limit: Some(200),
            });
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let projection = projection_meta(&connection)?;
        Ok(HealthSummaryResponse {
            as_of: rfc3339(now_ns)?,
            count: Uint64(count.try_into().unwrap_or(u64::MAX)),
            projection: trellis_runtime_apis::types::HealthQueryResponseprojection {
                last_stream_sequence: Uint64(
                    projection
                        .last_stream_sequence
                        .try_into()
                        .unwrap_or_default(),
                ),
                revision: Uint64(projection.revision.try_into().unwrap_or_default()),
                gap_detected: projection.gap_detected,
                retained_from: projection.retained_from,
                complete_since: projection.complete_since,
            },
        })
    }

    pub(crate) fn inspect(
        &self,
        request: &HealthInspectRequest,
        now_ns: i64,
    ) -> Result<Option<HealthInspectResponse>, HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let mut rows = load_latest(&connection)?
            .into_iter()
            .filter(|row| {
                row.participant_kind == request.participant_kind.as_str()
                    && row.contract_id == request.contract_id.as_ref()
                    && request
                        .instance_id
                        .as_ref()
                        .is_none_or(|instance_id| row.instance_id == instance_id.as_ref())
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Ok(None);
        }
        rows.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));

        let mut online_instances = 0;
        let mut offline_instances = 0;
        let mut has_degraded = false;
        let mut has_unhealthy = false;
        let mut participant_name = rows[0].participant_name.clone();
        let mut participant_name_observed_at = rows[0].observed_at_ns;
        let mut instances = Vec::with_capacity(rows.len());
        for row in &rows {
            let effective_status = if row.heartbeat_deadline_ns <= now_ns {
                "offline"
            } else {
                row.effective_status.as_str()
            };
            if effective_status == "offline" {
                offline_instances += 1;
            } else {
                online_instances += 1;
                has_degraded |= effective_status == "degraded";
                has_unhealthy |= effective_status == "unhealthy";
            }
            if row.observed_at_ns > participant_name_observed_at {
                participant_name = row.participant_name.clone();
                participant_name_observed_at = row.observed_at_ns;
            }
            let latest_sample = serde_json::from_str::<
                HealthInspectResponseInstancesItemLatestSample,
            >(&row.latest_sample_json)?;
            instances.push(HealthInspectResponseInstancesItem {
                instance_id: row.instance_id.clone(),
                deployment_id: row.deployment_id.clone(),
                contract_digest: row.contract_digest.clone(),
                reported_status: wire(row.reported_status.clone())?,
                effective_status: wire(effective_status)?,
                observed_at: rfc3339(row.observed_at_ns)?,
                heartbeat_deadline: rfc3339(row.heartbeat_deadline_ns)?,
                age_ms: Uint64(
                    (now_ns.saturating_sub(row.observed_at_ns) / 1_000_000)
                        .try_into()
                        .unwrap_or_default(),
                ),
                started_at: row.started_at.clone(),
                latest_sample,
            });
        }
        let effective_status = if online_instances == 0 {
            "offline"
        } else if has_unhealthy {
            "unhealthy"
        } else if has_degraded || offline_instances > 0 {
            "degraded"
        } else {
            "healthy"
        };

        let history_since_ns = request
            .history_since
            .as_deref()
            .map(parse_rfc3339_ns)
            .transpose()?
            .unwrap_or(i64::MIN);
        let history_limit = request
            .history_limit
            .map(|value| value.0 .0)
            .unwrap_or(100)
            .clamp(1, 500);
        let mut statement = connection.prepare(
            "SELECT interval_id, instance_id, started_at_ns, ended_at_ns, reported_status,
                    effective_status, checks_json, reason
             FROM health_status_intervals
             WHERE participant_kind = ?1 AND contract_id = ?2
               AND (?3 IS NULL OR instance_id = ?3)
               AND COALESCE(ended_at_ns, ?4) >= ?5
             ORDER BY started_at_ns DESC, interval_id DESC LIMIT ?6",
        )?;
        let history = statement
            .query_map(
                params![
                    request.participant_kind.as_str(),
                    request.contract_id.as_ref(),
                    request.instance_id.as_ref().map(AsRef::<str>::as_ref),
                    now_ns,
                    history_since_ns,
                    history_limit
                ],
                |row| {
                    let checks_json: String = row.get(6)?;
                    let checks =
                        serde_json::from_str::<Vec<HealthHeartbeatSampleChecksItem>>(&checks_json)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    checks_json.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?;
                    Ok(HealthInspectResponseHistoryItem {
                        interval_id: wire_from_sql(row.get::<_, i64>(0)?.to_string())?,
                        instance_id: row.get(1)?,
                        started_at: rfc3339(row.get(2)?).map_err(to_from_sql_error)?,
                        ended_at: row
                            .get::<_, Option<i64>>(3)?
                            .map(rfc3339)
                            .transpose()
                            .map_err(to_from_sql_error)?,
                        reported_status: wire_from_sql(row.get(4)?)?,
                        effective_status: wire_from_sql(row.get(5)?)?,
                        checks: checks
                            .into_iter()
                            .map(|check| {
                                Ok(HealthInspectResponseHistoryItemChecksItem {
                                    name: check.name.to_string(),
                                    status: wire_from_sql(check.status.as_str().to_string())?,
                                })
                            })
                            .collect::<Result<Vec<_>, rusqlite::Error>>()?,
                        reason: wire_from_sql(row.get(7)?)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        let projection = projection_meta(&connection)?;
        Ok(Some(HealthInspectResponse {
            participant: HealthInspectResponseParticipant {
                participant_kind: wire(&request.participant_kind)?,
                contract_id: request.contract_id.to_string(),
                participant_name,
                effective_status: wire(effective_status)?,
                online_instances: Uint64(online_instances),
                offline_instances: Uint64(offline_instances),
            },
            instances,
            history,
            as_of: rfc3339(now_ns)?,
            projection: HealthInspectResponseProjection {
                last_stream_sequence: Uint64(
                    projection
                        .last_stream_sequence
                        .try_into()
                        .unwrap_or_default(),
                ),
                revision: Uint64(projection.revision.try_into().unwrap_or_default()),
                gap_detected: projection.gap_detected,
                retained_from: projection.retained_from,
                complete_since: projection.complete_since,
            },
        }))
    }

    pub(crate) fn metrics(
        &self,
        request: &HealthMetricsRequest,
        now_ns: i64,
    ) -> Result<HealthMetricsResponse, HealthStoreError> {
        let start_ns = parse_rfc3339_ns(&request.start)?;
        let end_ns = parse_rfc3339_ns(&request.end)?;
        let step_ns = request
            .step_ms
            .0
             .0
            .checked_mul(1_000_000)
            .ok_or(HealthStoreError::TimestampRange)?;
        if start_ns >= end_ns || step_ns < 300_000_000_000 {
            return Err(HealthStoreError::TimestampRange);
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let mut instance_ids = load_latest(&connection)?
            .into_iter()
            .filter(|row| {
                row.participant_kind == request.participant_kind.as_str()
                    && row.contract_id == request.contract_id.as_ref()
                    && matches_filter(request.instance_ids.as_ref(), &row.instance_id)
            })
            .map(|row| row.instance_id)
            .collect::<Vec<_>>();
        instance_ids.sort();
        instance_ids.dedup();

        let bucket_count = usize::try_from((end_ns - start_ns + step_ns - 1) / step_ns)
            .map_err(|_| HealthStoreError::TimestampRange)?;
        if bucket_count.saturating_mul(instance_ids.len()) > 1_000 {
            return Err(HealthStoreError::TimestampRange);
        }

        let mut summary_observed_ms = 0_i64;
        let mut summary_online_ms = 0_i64;
        let mut summary_samples = 0_i64;
        let mut summary_transitions = 0_i64;
        let mut series = Vec::with_capacity(instance_ids.len());
        for instance_id in instance_ids {
            let mut buckets = (0..bucket_count)
                .map(|index| {
                    let bucket_start =
                        start_ns + step_ns * i64::try_from(index).unwrap_or(i64::MAX);
                    MetricBucket::new(bucket_start, (bucket_start + step_ns).min(end_ns))
                })
                .collect::<Vec<_>>();

            let mut interval_statement = connection.prepare(
                "SELECT started_at_ns, COALESCE(ended_at_ns, ?4), effective_status
                 FROM health_status_intervals
                 WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3
                   AND started_at_ns < ?4 AND COALESCE(ended_at_ns, ?4) > ?5",
            )?;
            let intervals = interval_statement
                .query_map(
                    params![
                        request.participant_kind.as_str(),
                        request.contract_id.as_ref(),
                        instance_id,
                        end_ns,
                        start_ns
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(interval_statement);
            summary_transitions +=
                i64::try_from(intervals.len().saturating_sub(1)).unwrap_or(i64::MAX);
            for (interval_start, interval_end, status) in intervals {
                for bucket in &mut buckets {
                    let overlap_ns =
                        interval_end.min(bucket.end_ns) - interval_start.max(bucket.start_ns);
                    if overlap_ns > 0 {
                        bucket.add_status(&status, overlap_ns / 1_000_000);
                    }
                }
            }

            let metric_start = start_ns - start_ns.rem_euclid(300_000_000_000);
            let mut sample_statement = connection.prepare(
                "SELECT bucket_start_ns, sample_count FROM health_metric_buckets
                 WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3
                   AND bucket_start_ns >= ?4 AND bucket_start_ns < ?5",
            )?;
            let sample_rows = sample_statement
                .query_map(
                    params![
                        request.participant_kind.as_str(),
                        request.contract_id.as_ref(),
                        instance_id,
                        metric_start,
                        end_ns
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(sample_statement);
            for (bucket_start, sample_count) in sample_rows {
                if let Some(bucket) = metric_bucket_mut(&mut buckets, bucket_start) {
                    bucket.sample_count += sample_count;
                    summary_samples += sample_count;
                }
            }

            let mut check_statement = connection.prepare(
                "SELECT bucket_start_ns, check_name, sample_count, ok_count, failed_count,
                        latency_sum_ms, latency_max_ms
                 FROM health_check_metric_buckets
                 WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3
                   AND bucket_start_ns >= ?4 AND bucket_start_ns < ?5",
            )?;
            let check_rows = check_statement
                .query_map(
                    params![
                        request.participant_kind.as_str(),
                        request.contract_id.as_ref(),
                        instance_id,
                        metric_start,
                        end_ns
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, f64>(5)?,
                            row.get::<_, f64>(6)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(check_statement);
            for (bucket_start, name, count, ok, failed, latency_sum, latency_max) in check_rows {
                if !matches_filter(request.check_names.as_ref(), &name) {
                    continue;
                }
                if let Some(bucket) = metric_bucket_mut(&mut buckets, bucket_start) {
                    let check = bucket.checks.entry(name).or_default();
                    check.sample_count += count;
                    check.ok_count += ok;
                    check.failed_count += failed;
                    check.latency_sum_ms += latency_sum;
                    check.latency_max_ms = check.latency_max_ms.max(latency_max);
                }
            }

            let response_buckets = buckets
                .into_iter()
                .map(|bucket| {
                    summary_observed_ms += bucket.observed_ms;
                    summary_online_ms +=
                        bucket.healthy_ms + bucket.degraded_ms + bucket.unhealthy_ms;
                    Ok(HealthMetricsResponseSeriesItemBucketsItem {
                        start: rfc3339(bucket.start_ns)?,
                        end: rfc3339(bucket.end_ns)?,
                        observed_ms: Uint64(bucket.observed_ms.try_into().unwrap_or_default()),
                        sample_count: Uint64(bucket.sample_count.try_into().unwrap_or_default()),
                        healthy_ms: Uint64(bucket.healthy_ms.try_into().unwrap_or_default()),
                        degraded_ms: Uint64(bucket.degraded_ms.try_into().unwrap_or_default()),
                        unhealthy_ms: Uint64(bucket.unhealthy_ms.try_into().unwrap_or_default()),
                        offline_ms: Uint64(bucket.offline_ms.try_into().unwrap_or_default()),
                        checks: bucket
                            .checks
                            .into_iter()
                            .map(|(name, check)| {
                                HealthMetricsResponseSeriesItemBucketsItemChecksItem {
                                    name,
                                    sample_count: Uint64(
                                        check.sample_count.try_into().unwrap_or_default(),
                                    ),
                                    ok_count: Uint64(check.ok_count.try_into().unwrap_or_default()),
                                    failed_count: Uint64(
                                        check.failed_count.try_into().unwrap_or_default(),
                                    ),
                                    latency_average_ms: if check.sample_count == 0 {
                                        Number(0.0).into()
                                    } else {
                                        Number(check.latency_sum_ms / check.sample_count as f64)
                                            .into()
                                    },
                                    latency_max_ms: Number(check.latency_max_ms).into(),
                                }
                            })
                            .collect(),
                    })
                })
                .collect::<Result<Vec<_>, HealthStoreError>>()?;
            series.push(HealthMetricsResponseSeriesItem {
                participant_kind: wire(&request.participant_kind)?,
                contract_id: request.contract_id.to_string(),
                instance_id,
                buckets: response_buckets,
            });
        }

        let projection = projection_meta(&connection)?;
        Ok(HealthMetricsResponse {
            series,
            summary: HealthMetricsResponseSummary {
                availability: (summary_observed_ms > 0)
                    .then(|| Number(summary_online_ms as f64 / summary_observed_ms as f64).into()),
                observed_ms: Uint64(summary_observed_ms.try_into().unwrap_or_default()),
                online_ms: Uint64(summary_online_ms.try_into().unwrap_or_default()),
                sample_count: Uint64(summary_samples.try_into().unwrap_or_default()),
                transitions: Uint64(summary_transitions.try_into().unwrap_or_default()),
            },
            as_of: rfc3339(now_ns)?,
            projection: HealthMetricsResponseProjection {
                last_stream_sequence: Uint64(
                    projection
                        .last_stream_sequence
                        .try_into()
                        .unwrap_or_default(),
                ),
                revision: Uint64(projection.revision.try_into().unwrap_or_default()),
                gap_detected: projection.gap_detected,
                retained_from: projection.retained_from,
                complete_since: projection.complete_since,
            },
        })
    }
}

#[derive(Debug)]
struct MetricBucket {
    start_ns: i64,
    end_ns: i64,
    observed_ms: i64,
    sample_count: i64,
    healthy_ms: i64,
    degraded_ms: i64,
    unhealthy_ms: i64,
    offline_ms: i64,
    checks: BTreeMap<String, CheckMetric>,
}

impl MetricBucket {
    fn new(start_ns: i64, end_ns: i64) -> Self {
        Self {
            start_ns,
            end_ns,
            observed_ms: 0,
            sample_count: 0,
            healthy_ms: 0,
            degraded_ms: 0,
            unhealthy_ms: 0,
            offline_ms: 0,
            checks: BTreeMap::new(),
        }
    }

    fn add_status(&mut self, status: &str, duration_ms: i64) {
        self.observed_ms += duration_ms;
        match status {
            "healthy" => self.healthy_ms += duration_ms,
            "degraded" => self.degraded_ms += duration_ms,
            "unhealthy" => self.unhealthy_ms += duration_ms,
            "offline" => self.offline_ms += duration_ms,
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
struct CheckMetric {
    sample_count: i64,
    ok_count: i64,
    failed_count: i64,
    latency_sum_ms: f64,
    latency_max_ms: f64,
}

fn metric_bucket_mut(buckets: &mut [MetricBucket], timestamp_ns: i64) -> Option<&mut MetricBucket> {
    buckets
        .iter_mut()
        .find(|bucket| timestamp_ns >= bucket.start_ns && timestamp_ns < bucket.end_ns)
}

fn matches_filter<T: AsRef<str>>(filter: Option<&Vec<T>>, value: &str) -> bool {
    filter.is_none_or(|filter| {
        filter.is_empty() || filter.iter().any(|candidate| candidate.as_ref() == value)
    })
}

fn query_digest(request: &HealthQueryRequest) -> Result<String, HealthStoreError> {
    let value = serde_json::json!({
        "endpoint": "health.Query",
        "participantKinds": request.participant_kinds,
        "contractIds": request.contract_ids,
        "deploymentIds": request.deployment_ids,
        "statuses": request.statuses,
        "search": request.search,
        "order": ["participantKind", "participantId"],
    });
    pagination_query_digest("health.Query", &value).map_err(|_| HealthStoreError::InvalidPagination)
}

fn decode_cursor(cursor: &str, query_digest: &str) -> Result<(String, String), HealthStoreError> {
    let after: (String, String) = decode_pagination_cursor(cursor, query_digest)
        .map_err(|_| HealthStoreError::InvalidPagination)?;
    if after.0.is_empty() || after.1.is_empty() {
        return Err(HealthStoreError::InvalidPagination);
    }
    Ok(after)
}

fn encode_cursor(
    query_digest: &str,
    entry: &HealthQueryResponseEntriesItem,
) -> Result<String, HealthStoreError> {
    encode_pagination_cursor(
        query_digest,
        &(
            entry.participant_kind.as_str().to_owned(),
            entry.contract_id.clone(),
        ),
    )
    .map_err(|_| HealthStoreError::InvalidPagination)
}

fn wire<T: DeserializeOwned, S: Serialize>(value: S) -> Result<T, HealthStoreError> {
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}

fn wire_from_sql<T: DeserializeOwned>(value: String) -> Result<T, rusqlite::Error> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| to_from_sql_error(error.into()))
}

fn load_latest(connection: &rusqlite::Connection) -> Result<Vec<LatestRow>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT participant_kind, contract_id, instance_id, deployment_id, participant_name,
                contract_digest, reported_status, effective_status, observed_at_ns,
                heartbeat_deadline_ns, started_at, runtime, runtime_version, version,
                latest_sample_json
         FROM health_latest ORDER BY participant_kind, contract_id, instance_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(LatestRow {
                participant_kind: row.get(0)?,
                contract_id: row.get(1)?,
                instance_id: row.get(2)?,
                deployment_id: row.get(3)?,
                participant_name: row.get(4)?,
                contract_digest: row.get(5)?,
                reported_status: row.get(6)?,
                effective_status: row.get(7)?,
                observed_at_ns: row.get(8)?,
                heartbeat_deadline_ns: row.get(9)?,
                started_at: row.get(10)?,
                runtime: row.get(11)?,
                version: row.get(13)?,
                latest_sample_json: row.get(14)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug)]
struct ProjectionMeta {
    last_stream_sequence: i64,
    revision: i64,
    gap_detected: bool,
    retained_from: Option<String>,
    complete_since: Option<String>,
}

fn projection_meta(connection: &rusqlite::Connection) -> Result<ProjectionMeta, HealthStoreError> {
    let row = connection.query_row(
        "SELECT last_stream_sequence, revision, gap_detected, retained_from_ns,
                complete_since_ns FROM health_projection_meta WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        },
    )?;
    Ok(ProjectionMeta {
        last_stream_sequence: row.0,
        revision: row.1,
        gap_detected: row.2,
        retained_from: row.3.map(rfc3339).transpose()?,
        complete_since: row.4.map(rfc3339).transpose()?,
    })
}

fn parse_rfc3339_ns(value: &str) -> Result<i64, HealthStoreError> {
    let timestamp =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|_| HealthStoreError::TimestampRange)?
            .unix_timestamp_nanos();
    i64::try_from(timestamp).map_err(|_| HealthStoreError::TimestampRange)
}

fn to_from_sql_error(error: HealthStoreError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(error))
}
