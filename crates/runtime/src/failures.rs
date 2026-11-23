//! The most recent server failures, kept in memory for `Auth.Runtime.Diagnostics`.
//!
//! Every failure the server shows a caller as a reference is logged once at
//! ERROR level with an `error_id` field: HTTP requests (see `http_failure`)
//! and RPC, live, and Operation failures alike. [`layer`] watches the process's
//! log for those lines and keeps the last [`CAPACITY`] of them: the ID, when,
//! the route, the code, and the error. It never keeps a line's other fields,
//! such as its cause or caller, so nothing the line leaves out reaches a
//! diagnostics reader either. The ring lives only as long as the process.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::Mutex;

use time::OffsetDateTime;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;

/// How many failures the process keeps.
pub const CAPACITY: usize = 50;

static RECENT: Mutex<VecDeque<Failure>> = Mutex::new(VecDeque::new());

/// One logged failure, as diagnostics reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The reference the caller was shown.
    pub id: String,
    /// When it was logged.
    pub at: OffsetDateTime,
    /// The HTTP method and path, or the RPC, live, or Operation name.
    pub route: Option<String>,
    /// The error code, or the error type when the line has no code.
    pub code: Option<String>,
    /// What failed.
    pub error: String,
}

/// A tracing layer that keeps the process's most recent failure lines.
///
/// Install it beside the log output, once per process.
#[must_use]
pub fn layer() -> FailureLayer {
    FailureLayer
}

/// The layer [`layer`] returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct FailureLayer;

impl<S: Subscriber> tracing_subscriber::Layer<S> for FailureLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if *event.metadata().level() != Level::ERROR {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Some(id) = fields.id else {
            return;
        };
        let failure = Failure {
            id,
            at: OffsetDateTime::now_utc(),
            route: fields.route,
            code: fields.code.or(fields.error_type),
            error: fields
                .error
                .or(fields.message)
                .unwrap_or_else(|| "the server failed".to_owned()),
        };
        let mut recent = RECENT.lock().unwrap_or_else(|error| error.into_inner());
        if recent.len() == CAPACITY {
            recent.pop_front();
        }
        recent.push_back(failure);
    }
}

/// The failures kept, newest first.
#[must_use]
pub fn recent() -> Vec<Failure> {
    RECENT
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .rev()
        .cloned()
        .collect()
}

#[derive(Default)]
struct Fields {
    id: Option<String>,
    route: Option<String>,
    code: Option<String>,
    error_type: Option<String>,
    error: Option<String>,
    message: Option<String>,
}

impl Fields {
    fn set(&mut self, field: &Field, value: String) {
        let slot = match field.name() {
            "error_id" => &mut self.id,
            "route" => &mut self.route,
            "code" => &mut self.code,
            "error_type" => &mut self.error_type,
            "error" => &mut self.error,
            "message" => &mut self.message,
            _ => return,
        };
        *slot = Some(value);
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.set(field, value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.set(field, format!("{value:?}"));
    }
}
