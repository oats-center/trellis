use crate::client::TrellisClient;
use crate::jobs::types::JobEvent;

/// Read one resource-owned job at a fixed broker watermark through the Jobs provider.
pub(crate) async fn read(
    client: &TrellisClient,
    queue: &str,
    job_id: &str,
) -> Result<Vec<(u64, JobEvent)>, String> {
    let mut next = 1_u64;
    let mut through = None;
    let mut events = Vec::new();
    loop {
        let input = crate::__types::trellis::JobsHistoryRequest {
            queue: queue.into(),
            job_id: job_id.into(),
            from_sequence: next.into(),
            through_sequence: through.map(Into::into),
            extra: Default::default(),
        };
        let value = client
            .request_json_value(
                "trellis.jobs@v1",
                trellis_protocol::CatalogActionIdentity {
                    kind: trellis_protocol::ApiSurfaceKind::Rpc,
                    name: "History".into(),
                    direction: trellis_protocol::PermissionAction::Call,
                },
                "rpc.v1.jobs.History",
                &serde_json::to_value(input).map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        let page: crate::__types::trellis::JobsHistoryResponse =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        if through.is_some_and(|through| through != page.through_sequence.0) {
            return Err("Jobs history watermark changed".into());
        }
        through = Some(page.through_sequence.0);
        for entry in page.entries {
            if entry.sequence.0 < next || entry.sequence.0 > page.through_sequence.0 {
                return Err("Jobs history sequence out of range".into());
            }
            next = entry
                .sequence
                .0
                .checked_add(1)
                .ok_or("Jobs history sequence overflow")?;
            events.push((
                entry.sequence.0,
                serde_json::from_slice(&entry.payload.0).map_err(|error| error.to_string())?,
            ));
        }
        match page.next_sequence {
            Some(cursor) if cursor.0 >= next && cursor.0 <= page.through_sequence.0 => {
                next = cursor.0
            }
            Some(_) => return Err("Jobs history cursor out of range".into()),
            None => return Ok(events),
        }
    }
}
