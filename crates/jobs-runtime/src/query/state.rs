use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::jobs::types::JobState;

use super::JobsQueryError;

pub(super) fn parse_state_filter<T: serde::Serialize>(
    state: Option<&Vec<T>>,
) -> Result<Option<Vec<JobState>>, JobsQueryError> {
    let Some(state) = state else {
        return Ok(None);
    };

    serde_json::from_value::<Vec<JobState>>(serde_json::to_value(state).map_err(|error| {
        JobsQueryError::Validation {
            field: "state",
            details: format!("must be an array of job states: {error}"),
        }
    })?)
    .map(Some)
    .map_err(|error| JobsQueryError::Validation {
        field: "state",
        details: format!("must be an array of job states: {error}"),
    })
}

pub(super) fn now_timestamp_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_state_filter_accepts_array_only() {
        let state = vec!["failed".to_string(), "dead".to_string()];
        let filter = parse_state_filter(Some(&state))
            .expect("array state filter should parse")
            .expect("state filter should be present");

        assert_eq!(filter, vec![JobState::Failed, JobState::Dead]);
    }

    #[test]
    fn parse_state_filter_rejects_invalid_state_array() {
        let state = vec!["failed".to_string(), "unknown".to_string()];
        let error = parse_state_filter(Some(&state)).expect_err("invalid state filter should fail");

        assert!(matches!(
            error,
            JobsQueryError::Validation { field: "state", .. }
        ));
    }
}
