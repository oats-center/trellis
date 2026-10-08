use std::sync::Arc;

use opentelemetry::KeyValue;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::telemetry::instruments::{add_counter, add_updown, CounterFamily, UpDownFamily};

/// Local provider intake limits shared by every endpoint and transport generation.
///
/// Bytes count retained inbound messages, not decoded application heap or NATS
/// buffers. Slots cover dispatch, not Operation execution or Live session lifetimes.
#[derive(Clone, Copy, Debug)]
pub struct RequestLimits {
    /// Maximum concurrent ordinary request dispatches, including authorization.
    pub requests: usize,
    /// Maximum retained ordinary inbound message bytes through reply handoff.
    pub bytes: u32,
    /// Reserved concurrency for built-in Operation get/cancel and Live controls.
    pub controls: usize,
    /// Reserved retained bytes for built-in Operation get/cancel and Live controls.
    pub control_bytes: u32,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            requests: 32,
            bytes: 16 * 1024 * 1024,
            controls: 8,
            control_bytes: 1024 * 1024,
        }
    }
}

impl RequestLimits {
    pub(crate) fn validate(self) -> Result<Self, super::ServerError> {
        if self.requests == 0
            || self.requests > Semaphore::MAX_PERMITS
            || self.controls == 0
            || self.controls > Semaphore::MAX_PERMITS
            || self.requests > 1_000_000
            || self.controls > 1_000_000
            || self.bytes == 0
            || self.control_bytes == 0
            || self.bytes as usize > Semaphore::MAX_PERMITS
            || self.control_bytes as usize > Semaphore::MAX_PERMITS
        {
            return Err(super::ServerError::InvalidRequestLimits);
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_remain_available_when_requests_and_refusal_verification_are_full() {
        let admission = RequestAdmission::new(RequestLimits {
            requests: 1,
            bytes: 100,
            controls: 1,
            control_bytes: 100,
        });
        let (request, refused) = admission.admit(10, false).unwrap();
        assert!(!refused);
        let refusals: Vec<_> = (0..4)
            .map(|_| {
                let (permit, refused) = admission.admit(10, false).unwrap();
                assert!(refused);
                permit
            })
            .collect();
        assert!(admission.admit(10, false).is_none());
        let (control, refused) = admission.admit(10, true).unwrap();
        assert!(!refused);
        drop((request, control, refusals));
        assert!(!admission.admit(10, false).unwrap().1);
        assert!(!admission.admit(10, true).unwrap().1);
    }

    #[test]
    fn byte_refusals_release_partial_count_and_unwinding_returns_capacity() {
        let admission = RequestAdmission::new(RequestLimits {
            requests: 2,
            bytes: 4,
            controls: 1,
            control_bytes: 4,
        });
        let (first, refused) = admission.admit(3, false).unwrap();
        assert!(!refused);
        let (rejected, refused) = admission.admit(2, false).unwrap();
        assert!(refused);
        let (last_byte, refused) = admission.admit(1, false).unwrap();
        assert!(
            !refused,
            "the failed byte acquisition must not retain a count slot"
        );
        drop((first, last_byte, rejected));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_permit, refused) = admission.admit(4, false).unwrap();
            assert!(!refused);
            panic!("unwind a reservation owner");
        }));
        assert!(unwound.is_err());
        assert!(!admission.admit(4, false).unwrap().1);
    }
}

#[derive(Debug)]
struct Pool {
    requests: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl Pool {
    fn new(requests: usize, bytes: u32) -> Self {
        Self {
            requests: Arc::new(Semaphore::new(requests)),
            bytes: Arc::new(Semaphore::new(bytes as usize)),
        }
    }

    fn reserve(&self, bytes: usize, kind: &'static str) -> Result<RequestPermit, &'static str> {
        let slot = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| "requests")?;
        let bytes_count = u32::try_from(bytes).map_err(|_| "bytes")?;
        let capacity = self
            .bytes
            .clone()
            .try_acquire_many_owned(bytes_count)
            .map_err(|_| "bytes")?;
        let attributes = [KeyValue::new("trellis.kind", kind)];
        add_updown(UpDownFamily::ServiceAdmissionInflight, 1, &attributes);
        add_updown(
            UpDownFamily::ServiceAdmissionBytes,
            bytes as i64,
            &attributes,
        );
        Ok(RequestPermit {
            _slot: slot,
            _capacity: capacity,
            bytes,
            kind,
        })
    }
}

/// Owned permits release capacity on reply completion, cancellation, or unwind.
pub(crate) struct RequestPermit {
    _slot: OwnedSemaphorePermit,
    _capacity: OwnedSemaphorePermit,
    bytes: usize,
    kind: &'static str,
}

impl Drop for RequestPermit {
    fn drop(&mut self) {
        let attributes = [KeyValue::new("trellis.kind", self.kind)];
        add_updown(UpDownFamily::ServiceAdmissionInflight, -1, &attributes);
        add_updown(
            UpDownFamily::ServiceAdmissionBytes,
            -(self.bytes as i64),
            &attributes,
        );
    }
}

#[derive(Debug)]
pub(crate) struct RequestAdmission {
    requests: Pool,
    controls: Pool,
    verification: Pool,
}

impl RequestAdmission {
    pub(crate) fn new(limits: RequestLimits) -> Self {
        Self {
            requests: Pool::new(limits.requests, limits.bytes),
            controls: Pool::new(limits.controls, limits.control_bytes),
            verification: Pool::new(4, 1024 * 1024),
        }
    }

    /// Refusals have a small separate verification allowance: authenticate and
    /// bind the reply before saying busy, or drop without a reflected response.
    pub(crate) fn admit(&self, bytes: usize, control: bool) -> Option<(RequestPermit, bool)> {
        let kind = if control { "control" } else { "request" };
        let pool = if control {
            &self.controls
        } else {
            &self.requests
        };
        match pool.reserve(bytes, kind) {
            Ok(permit) => Some((permit, false)),
            Err(reason) => {
                let verification = self.verification.reserve(bytes, "verification");
                let reason = match &verification {
                    Ok(_) => reason,
                    Err("bytes") => "verification_bytes",
                    Err(_) => "verification_requests",
                };
                add_counter(
                    CounterFamily::ServiceAdmissionRejections,
                    1,
                    &[
                        KeyValue::new("trellis.kind", kind),
                        KeyValue::new("trellis.reason", reason),
                    ],
                );
                verification.ok().map(|permit| (permit, true))
            }
        }
    }
}
