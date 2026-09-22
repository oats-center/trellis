const BUILTIN_JOBS_STREAM: &str = "JOBS";
const BUILTIN_JOBS_ADVISORIES_STREAM: &str = "JOBS_ADVISORIES";

/// Resolved admin-side resources needed by projector, janitor, and advisory loops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsAdminResources {
    pub jobs_stream: String,
    pub jobs_advisories_stream: String,
}

/// Extract all Jobs admin resource names from a resolved binding payload.
pub fn jobs_admin_resources() -> JobsAdminResources {
    JobsAdminResources {
        jobs_stream: BUILTIN_JOBS_STREAM.to_string(),
        jobs_advisories_stream: BUILTIN_JOBS_ADVISORIES_STREAM.to_string(),
    }
}
