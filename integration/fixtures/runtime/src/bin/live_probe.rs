//! Case-owned Live observation acceptance provider.
//!
//! One process serves the `liveprobe@v1` API: `Inspect`/`Release` control a
//! per-`runId/streamId` scalar source, and `Watch` streams signed application
//! frames that advance only when a release permits them. The source starts only
//! when its Feed is actually activated, waits on an owned notification while
//! quiet, and runs exactly one cleanup when its stream ends or is dropped.

use futures_util::{stream, Stream, StreamExt};
use runtime_trellis::participants::runtime_trellis_live_probe_provider::{Participant, Provider};
use runtime_trellis::types::{LiveProbeFrame, LiveProbeKey, LiveProbeStatus};
use runtime_trellis::Int64;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use trellis_rs::service::{ServerError, ServiceConnectOptions};
use trellis_rs::LiveCancellation;

const MAX_THROUGH_INDEX: i64 = 1_026;
const MAX_KEY_COUNT: usize = 16;

#[derive(Default)]
struct Counters {
    starts: i64,
    active: i64,
    cleanups: i64,
    emitted: i64,
}

struct Release {
    through_index: i64,
    finish: bool,
    fail: bool,
    payload_bytes: u32,
    padding_bytes: u32,
}

impl Default for Release {
    fn default() -> Self {
        Self {
            through_index: 0,
            finish: false,
            fail: false,
            payload_bytes: 4,
            padding_bytes: 0,
        }
    }
}

struct KeyState {
    counters: Mutex<Counters>,
    release: Mutex<Release>,
    notify: Notify,
    next_generation: AtomicI64,
}

impl KeyState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            counters: Mutex::new(Counters::default()),
            release: Mutex::new(Release::default()),
            notify: Notify::new(),
            next_generation: AtomicI64::new(0),
        })
    }

    fn snapshot(&self) -> (i64, bool, bool, u32, u32) {
        let release = self.release.lock().expect("release lock");
        (
            release.through_index,
            release.finish,
            release.fail,
            release.payload_bytes,
            release.padding_bytes,
        )
    }

    fn start(&self) -> i64 {
        let mut counters = self.counters.lock().expect("counters lock");
        counters.starts += 1;
        counters.active += 1;
        self.next_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn record_emitted(&self) {
        self.counters.lock().expect("counters lock").emitted += 1;
    }

    fn cleanup(&self) {
        let mut counters = self.counters.lock().expect("counters lock");
        counters.cleanups += 1;
        counters.active -= 1;
    }

    fn status(&self) -> LiveProbeStatus {
        let counters = self.counters.lock().expect("counters lock");
        LiveProbeStatus {
            starts: Int64(counters.starts),
            active: Int64(counters.active),
            cleanups: Int64(counters.cleanups),
            emitted: Int64(counters.emitted),
            extra: Default::default(),
        }
    }

    fn released(
        &self,
        through_index: i64,
        finish: bool,
        fail: bool,
        payload: u32,
        padding: u32,
    ) -> Result<(), ServerError> {
        if !(0..=MAX_THROUGH_INDEX).contains(&through_index) {
            return Err(ServerError::Nats("throughIndex out of range".to_owned()));
        }
        let emitted = self.counters.lock().expect("counters lock").emitted;
        let mut release = self.release.lock().expect("release lock");
        if through_index < release.through_index {
            return Err(ServerError::Nats(
                "throughIndex must not regress".to_owned(),
            ));
        }
        let sizes_changed = payload != release.payload_bytes || padding != release.padding_bytes;
        if sizes_changed && emitted < release.through_index {
            return Err(ServerError::Nats(
                "size settings may change only after the released range has emitted".to_owned(),
            ));
        }
        release.through_index = through_index;
        release.finish = finish;
        release.fail = fail;
        release.payload_bytes = payload;
        release.padding_bytes = padding;
        drop(release);
        self.notify.notify_waiters();
        Ok(())
    }
}

type Registry = Arc<Mutex<HashMap<(String, String), Arc<KeyState>>>>;

fn key_of(input: &LiveProbeKey) -> (String, String) {
    (input.run_id.clone(), input.stream_id.clone())
}

fn source_stream(
    state: Arc<KeyState>,
    cancellation: LiveCancellation,
) -> impl Stream<Item = Result<LiveProbeFrame, ServerError>> {
    let cursor = SourceCursor {
        state,
        generation: 0,
        next_index: 1,
        finished: false,
        cancellation,
    };
    stream::unfold(cursor, |mut cursor| async move {
        loop {
            if cursor.cancellation.is_cancelled() {
                return None;
            }
            if cursor.generation == 0 {
                cursor.generation = cursor.state.start();
            }
            let (through, finish, fail, payload_bytes, padding_bytes) = cursor.state.snapshot();
            if cursor.next_index <= through {
                let frame = LiveProbeFrame {
                    run_id: String::new(),
                    stream_id: String::new(),
                    source_generation: Int64(cursor.generation),
                    index: Int64(cursor.next_index),
                    payload: vec![0_u8; payload_bytes as usize].into(),
                    padding: "p".repeat(padding_bytes as usize),
                    extra: Default::default(),
                };
                cursor.state.record_emitted();
                cursor.next_index += 1;
                return Some((Ok(frame), cursor));
            }
            if finish {
                cursor.finish();
                return None;
            }
            if fail {
                return Some((
                    Err(ServerError::Nats("live probe source failed".to_owned())),
                    cursor,
                ));
            }
            tokio::select! {
                _ = cursor.state.notify.notified() => {}
                _ = cursor.cancellation.cancelled() => {}
            }
        }
    })
}

struct SourceCursor {
    state: Arc<KeyState>,
    generation: i64,
    next_index: i64,
    finished: bool,
    cancellation: LiveCancellation,
}

impl SourceCursor {
    fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            self.state.cleanup();
        }
    }
}

impl Drop for SourceCursor {
    fn drop(&mut self) {
        if self.generation != 0 {
            self.finish();
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TRELLIS_URL")?;
    let identity = std::env::var("TRELLIS_IDENTITY_SEED")?;
    // The outage acceptance case keeps a refresh/reconnect alive across a
    // deliberately unavailable transport; opt into a longer ordinary SDK
    // timeout without changing the production default.
    let mut options = ServiceConnectOptions::new(&url, &identity);
    if let Ok(raw_timeout_ms) = std::env::var("TRELLIS_TIMEOUT_MS") {
        options = options.with_timeout_ms(raw_timeout_ms.parse()?);
    }
    let mut service = Participant::connect(options).await?;
    let mut provider = Provider::new(&mut service);
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

    {
        let registry = Arc::clone(&registry);
        provider
            .runtime_trellis_liveprobe_v1()
            .register_inspect(move |_, input| {
                let registry = Arc::clone(&registry);
                async move {
                    let status = registry
                        .lock()
                        .expect("registry lock")
                        .get(&key_of(&input))
                        .map_or_else(
                            || LiveProbeStatus {
                                starts: Int64(0),
                                active: Int64(0),
                                cleanups: Int64(0),
                                emitted: Int64(0),
                                extra: Default::default(),
                            },
                            |state| state.status(),
                        );
                    Ok(status)
                }
            });
    }
    {
        let registry = Arc::clone(&registry);
        provider
            .runtime_trellis_liveprobe_v1()
            .register_release(move |_, input| {
                let registry = Arc::clone(&registry);
                async move {
                    let state = {
                        let mut guard = registry.lock().expect("registry lock");
                        let key = (input.run_id.clone(), input.stream_id.clone());
                        if !guard.contains_key(&key) && guard.len() >= MAX_KEY_COUNT {
                            return Err(ServerError::Nats("too many live probe keys".to_owned()));
                        }
                        Arc::clone(guard.entry(key).or_insert_with(KeyState::new))
                    };
                    state.released(
                        input.through_index.0,
                        input.finish,
                        input.fail,
                        input.payload_bytes.unwrap_or(4),
                        input.padding_bytes.unwrap_or(0),
                    )?;
                    Ok(state.status())
                }
            });
    }
    provider
        .runtime_trellis_liveprobe_v1()
        .register_watch(move |context, input| {
            let state = {
                let mut guard = registry.lock().expect("registry lock");
                Arc::clone(
                    guard
                        .entry((input.run_id.clone(), input.stream_id.clone()))
                        .or_insert_with(KeyState::new),
                )
            };
            let stream = source_stream(state.clone(), context.cancellation.clone());
            let run_id = input.run_id.clone();
            let stream_id = input.stream_id.clone();
            stream.map(move |item| {
                item.map(|mut frame| {
                    frame.run_id = run_id.clone();
                    frame.stream_id = stream_id.clone();
                    frame
                })
            })
        });
    service.run().await?;
    Ok(())
}
