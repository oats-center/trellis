use crate::jobs::types::{Job, JobErrorDetail, JobEvent, JobEventType, JobState};

#[doc = concat!("Trellis API operation `", stringify!(is_terminal), "`.")]
pub fn is_terminal(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed
            | JobState::Failed
            | JobState::Cancelled
            | JobState::Expired
            | JobState::Skipped
            | JobState::Stale
            | JobState::Dead
            | JobState::Dismissed
    )
}

#[doc = concat!("Trellis API operation `", stringify!(job_from_work_event), "`.")]
pub fn job_from_work_event(event: &JobEvent) -> Option<Job> {
    match event.event_type {
        JobEventType::Created | JobEventType::Retried => seed_job_from_event(event),
        _ => None,
    }
}

#[doc = concat!("Trellis API operation `", stringify!(reduce_job_event), "`.")]
pub fn reduce_job_event(current: Option<&Job>, event: &JobEvent) -> Option<Job> {
    let current = match current {
        Some(value) => value,
        None => {
            if event.event_type != JobEventType::Created {
                return None;
            }
            return seed_job_from_event(event);
        }
    };

    if !is_legal_transition(current, event) {
        return Some(current.clone());
    }

    let mut next = Job {
        id: current.id.clone(),
        context: current.context.clone(),
        service: current.service.clone(),
        job_type: current.job_type.clone(),
        state: event.state,
        payload: current.payload.clone(),
        result: current.result.clone(),
        created_at: current.created_at.clone(),
        updated_at: event.timestamp.clone(),
        started_at: current.started_at.clone(),
        completed_at: current.completed_at.clone(),
        tries: event.tries,
        max_tries: event.max_tries.unwrap_or(current.max_tries),
        last_error: current.last_error.clone(),
        error_detail: current.error_detail.clone(),
        deadline: current.deadline.clone(),
        progress: current.progress.clone(),
        logs: current.logs.clone(),
        concurrency: current.concurrency.clone(),
        queue_policy: current.queue_policy.clone(),
        trigger: current.trigger.clone(),
        lineage: current.lineage.clone(),
        waiting_on: current.waiting_on.clone(),
    };

    if let Some(concurrency) = &event.concurrency {
        next.concurrency = Some(concurrency.clone());
    }
    if let Some(queue_policy) = &event.queue_policy {
        next.queue_policy = Some(queue_policy.clone());
    }
    if let Some(trigger) = &event.trigger {
        next.trigger = Some(trigger.clone());
    }
    if let Some(lineage) = &event.lineage {
        next.lineage = Some(lineage.clone());
    }

    match event.event_type {
        JobEventType::Started => {
            next.started_at = Some(event.timestamp.clone());
        }
        JobEventType::Progress => {
            next.progress = event.progress.clone();
        }
        JobEventType::Logged => {
            let mut logs = next.logs.take().unwrap_or_default();
            if let Some(new_logs) = &event.logs {
                logs.extend(new_logs.iter().cloned());
            }
            next.logs = Some(logs);
        }
        JobEventType::Waiting => {
            if let Some(wait_edge) = &event.wait_edge {
                let mut waiting_on = next.waiting_on.take().unwrap_or_default();
                waiting_on.retain(|edge| edge.id != wait_edge.id);
                waiting_on.push(wait_edge.clone());
                next.waiting_on = Some(waiting_on);
            }
        }
        JobEventType::Resumed => {
            if let Some(wait_edge) = &event.wait_edge {
                let mut waiting_on = next.waiting_on.take().unwrap_or_default();
                waiting_on.retain(|edge| edge.id != wait_edge.id);
                next.waiting_on = (!waiting_on.is_empty()).then_some(waiting_on);
            }
        }
        JobEventType::Completed => {
            next.result = event.result.clone();
            next.completed_at = Some(event.timestamp.clone());
            next.waiting_on = None;
        }
        JobEventType::Failed
        | JobEventType::Cancelled
        | JobEventType::Expired
        | JobEventType::Skipped
        | JobEventType::Stale
        | JobEventType::Dead
        | JobEventType::Dismissed => {
            next.last_error = event.error.clone();
            next.error_detail = error_detail_from_event(event);
            next.completed_at = Some(event.timestamp.clone());
            next.waiting_on = None;
        }
        JobEventType::Heartbeat => {}
        JobEventType::StaleCompletionIgnored => {
            next.last_error = event.error.clone();
            next.error_detail = error_detail_from_event(event);
        }
        JobEventType::Retry => {
            next.last_error = event.error.clone();
            next.error_detail = error_detail_from_event(event);
        }
        JobEventType::Retried => {
            if let Some(payload) = &event.payload {
                next.payload = payload.clone();
            }
            if let Some(deadline) = &event.deadline {
                next.deadline = Some(deadline.clone());
            }
            next.result = None;
            next.completed_at = None;
            next.started_at = None;
            next.last_error = None;
            next.error_detail = None;
            next.progress = None;
            next.logs = None;
            next.concurrency = event.concurrency.clone();
            next.queue_policy = event.queue_policy.clone();
            next.waiting_on = None;
            next.tries = 0;
        }
        JobEventType::Created => {}
    }

    Some(next)
}

fn is_legal_transition(current: &Job, event: &JobEvent) -> bool {
    match event.event_type {
        JobEventType::Created => false,
        JobEventType::Started => {
            matches!(current.state, JobState::Pending | JobState::Retry)
                && event.previous_state == Some(current.state)
        }
        JobEventType::Retry => {
            current.state == JobState::Active && event.previous_state == Some(JobState::Active)
        }
        JobEventType::Progress | JobEventType::Logged | JobEventType::Completed => {
            current.state == JobState::Active && event.previous_state == Some(JobState::Active)
        }
        JobEventType::Waiting | JobEventType::Resumed => {
            current.state == JobState::Active && event.previous_state == Some(JobState::Active)
        }
        JobEventType::Failed => {
            current.state == JobState::Active && event.previous_state == Some(JobState::Active)
        }
        JobEventType::Cancelled => {
            matches!(
                current.state,
                JobState::Pending | JobState::Retry | JobState::Active
            ) && event.previous_state == Some(current.state)
        }
        JobEventType::Expired => {
            matches!(
                current.state,
                JobState::Pending | JobState::Retry | JobState::Active
            ) && event.previous_state == Some(current.state)
        }
        JobEventType::Skipped => {
            matches!(current.state, JobState::Pending | JobState::Retry)
                && event.previous_state == Some(current.state)
        }
        JobEventType::Stale => {
            current.state == JobState::Active && event.previous_state == Some(JobState::Active)
        }
        JobEventType::Heartbeat | JobEventType::StaleCompletionIgnored => {
            current.state == JobState::Active && event.previous_state == Some(JobState::Active)
        }
        JobEventType::Retried => {
            matches!(current.state, JobState::Failed | JobState::Dead)
                && event.previous_state == Some(current.state)
        }
        JobEventType::Dead => {
            matches!(
                current.state,
                JobState::Active | JobState::Retry | JobState::Failed | JobState::Expired
            ) && event.previous_state == Some(current.state)
        }
        JobEventType::Dismissed => {
            current.state == JobState::Dead && event.previous_state == Some(JobState::Dead)
        }
    }
}

fn seed_job_from_event(event: &JobEvent) -> Option<Job> {
    let payload = event.payload.clone()?;
    Some(Job {
        id: event.job_id.clone(),
        context: event.context.clone(),
        service: event.service.clone(),
        job_type: event.job_type.clone(),
        state: event.state,
        payload,
        result: None,
        created_at: event.timestamp.clone(),
        updated_at: event.timestamp.clone(),
        started_at: None,
        completed_at: None,
        tries: event.tries,
        max_tries: event.max_tries.unwrap_or(1),
        last_error: None,
        error_detail: None,
        deadline: event.deadline.clone(),
        progress: None,
        logs: None,
        concurrency: event.concurrency.clone(),
        queue_policy: event.queue_policy.clone(),
        trigger: event.trigger.clone(),
        lineage: event.lineage.clone(),
        waiting_on: None,
    })
}

fn error_detail_from_event(event: &JobEvent) -> Option<JobErrorDetail> {
    event.error_detail.clone().or_else(|| {
        event
            .error
            .as_deref()
            .map(|message| JobErrorDetail::from_message(&event.service, &event.job_type, message))
    })
}
