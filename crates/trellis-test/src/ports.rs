//! Local port reservation for a test runtime.
//!
//! A listener is held until the caller releases it, so two runtimes starting in the same process
//! cannot pick the same port between selection and bind.

use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

use crate::error::TrellisTestError;

static HELD: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();

fn held() -> &'static Mutex<HashSet<u16>> {
    HELD.get_or_init(|| Mutex::new(HashSet::new()))
}

/// A reserved localhost port. Dropping it releases the port for reuse.
#[derive(Debug)]
pub(crate) struct ReservedPort {
    port: u16,
    listener: Option<TcpListener>,
}

impl ReservedPort {
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Releases the reservation so the runtime can bind the port itself.
    pub(crate) fn release_for_bind(&mut self) {
        if let Some(listener) = self.listener.take() {
            drop(listener);
        }
        if let Ok(mut held) = held().lock() {
            held.remove(&self.port);
        }
    }
}

impl Drop for ReservedPort {
    fn drop(&mut self) {
        self.release_for_bind();
    }
}

/// Reserves one free localhost port.
pub(crate) fn reserve_local_port() -> Result<ReservedPort, TrellisTestError> {
    for _ in 0..64 {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let mut held = held()
            .lock()
            .map_err(|_| TrellisTestError::InvalidState("port registry poisoned".to_owned()))?;
        if held.insert(port) {
            drop(held);
            return Ok(ReservedPort {
                port,
                listener: Some(listener),
            });
        }
    }
    Err(TrellisTestError::InvalidState(
        "no free local port could be reserved".to_owned(),
    ))
}
