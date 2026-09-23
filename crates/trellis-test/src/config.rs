//! Generates the bootstrap bundle and path isolation a test runtime runs against.
//!
//! The bundle is the same one `trellis init config` renders, so a test runtime is configured
//! exactly like a host runtime rather than through harness-only shortcuts.

use std::fs;
use std::path::{Path, PathBuf};

use trellis_bootstrap::{generate_trellis_bootstrap, TrellisBootstrapOptions};

use crate::error::TrellisTestError;
use crate::ports::ReservedPort;

/// Paths a started runtime needs after the bundle is written.
pub(crate) struct TrellisTestBundle {
    /// Generated `config.toml`.
    pub(crate) config_path: PathBuf,
    /// Generated `nats` source directory holding `nats.conf` and `jwt.conf`.
    pub(crate) nats_source: PathBuf,
    /// Directory holding the generated NATS state (resolver data, rendered config).
    pub(crate) nats_state: PathBuf,
    /// Directory holding the NATS binary cache when a pinned binary must be fetched.
    pub(crate) nats_cache: PathBuf,
    /// NATS pid file used for ownership checks.
    pub(crate) nats_pid: PathBuf,
    /// NATS log file.
    pub(crate) nats_log: PathBuf,
}

/// Writes the runtime bundle into `workdir/trellis` and isolates all mutable paths beneath
/// `workdir`.
pub(crate) fn generate_bundle(
    workdir: &Path,
    name: &str,
    http: &ReservedPort,
    nats: &ReservedPort,
    monitor: &ReservedPort,
    websocket: &ReservedPort,
    extra_origins: &[String],
) -> Result<TrellisTestBundle, TrellisTestError> {
    let out = workdir.join("trellis");
    let mut options = TrellisBootstrapOptions::new(out.clone());
    options.force = true;
    options.runtime.name = name.to_owned();
    options.runtime.trellis_port = http.port();
    options.runtime.nats_server_url = format!("nats://127.0.0.1:{}", nats.port());
    options.runtime.nats_websocket_url = format!("ws://localhost:{}", websocket.port());
    options.runtime.public_origin = format!("http://localhost:{}", http.port());
    options.runtime.extra_origins = extra_origins.to_vec();
    options.nats.nats_port = nats.port();
    options.nats.monitor_port = monitor.port();
    options.nats.websocket_port = websocket.port();
    generate_trellis_bootstrap(&options)
        .map_err(|error| TrellisTestError::Bootstrap(error.to_string()))?;

    let config_path = out.join("config.toml");
    let mut config = fs::read_to_string(&config_path)?;
    config.push_str(&format!(
        "\n[paths]\ndata = {data:?}\nstate = {state:?}\ncache = {cache:?}\nruntime = {runtime:?}\nlogs = {logs:?}\n",
        data = workdir.join("data").display().to_string(),
        state = workdir.join("state").display().to_string(),
        cache = workdir.join("cache").display().to_string(),
        runtime = workdir.join("run").display().to_string(),
        logs = workdir.join("logs").display().to_string(),
    ));
    fs::write(&config_path, config)?;

    Ok(TrellisTestBundle {
        config_path,
        nats_source: out.join("nats"),
        nats_state: workdir.join("state").join("nats"),
        nats_cache: workdir.join("cache").join("nats"),
        nats_pid: workdir.join("run").join("nats-server.pid"),
        nats_log: workdir.join("logs").join("nats-server.log"),
    })
}
