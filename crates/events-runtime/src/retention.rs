//! Policy retention for recorded events and settled dead letters.

use std::time::Duration;

use time::OffsetDateTime;
use trellis_rs::service::ServerError;

use crate::dead_letters::DeadLetterJournal;
use crate::storage::{EventsStore, EventsStoreError};

/// Rows removed per SQLite transaction, so a large backlog never holds the
/// projection's writer for long.
pub const RETENTION_BATCH: u64 = 1_000;

/// What one retention pass removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetentionPass {
    /// Recorded events removed.
    pub events: u64,
    /// Resolved or dismissed dead letters removed with their journal records.
    pub dead_letters: u64,
}

/// Remove recorded events older than `history` and resolved or dismissed
/// dead letters unchanged for `dead_letters`, in bounded batches.
///
/// Each event batch is one transaction. Each dead letter's journal records are
/// purged before its projection rows are dropped, so an interrupted pass
/// leaves the entry for the next one, and an entry a replay request reopened
/// meanwhile is kept.
///
/// # Errors
///
/// Returns the first SQLite or JetStream failure; work already committed stays.
pub async fn apply_retention(
    store: &EventsStore,
    journal: &DeadLetterJournal,
    history: Duration,
    dead_letters: Duration,
) -> Result<RetentionPass, ServerError> {
    let now = OffsetDateTime::now_utc();
    let mut pass = RetentionPass::default();
    let history_cutoff = now - history;
    loop {
        let batch_store = store.clone();
        let removed =
            blocking(move || batch_store.prune_history(history_cutoff, RETENTION_BATCH)).await?;
        pass.events += removed;
        if removed < RETENTION_BATCH {
            break;
        }
    }
    let dead_letter_cutoff = now - dead_letters;
    loop {
        let batch_store = store.clone();
        let settled = blocking(move || {
            batch_store.settled_dead_letters_before(dead_letter_cutoff, RETENTION_BATCH)
        })
        .await?;
        let mut removed = 0;
        for (id, journal_sequence) in &settled {
            if !journal
                .purge_through(id, *journal_sequence)
                .await
                .map_err(|error| ServerError::Internal(error.to_string()))?
            {
                continue;
            }
            let row_store = store.clone();
            let id = id.clone();
            let journal_sequence = *journal_sequence;
            if blocking(move || row_store.remove_settled_dead_letter(&id, journal_sequence)).await?
            {
                removed += 1;
            }
        }
        pass.dead_letters += removed;
        if (settled.len() as u64) < RETENTION_BATCH || removed == 0 {
            break;
        }
    }
    Ok(pass)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, EventsStoreError> + Send + 'static,
) -> Result<T, ServerError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ServerError::Internal(error.to_string()))?
        .map_err(ServerError::from)
}
