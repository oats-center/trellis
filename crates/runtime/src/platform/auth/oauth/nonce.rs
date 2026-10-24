//! One issuer/token-endpoint nonce window shared by all clients and tabs.

use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

pub(super) struct DpopNonces {
    current: (String, Instant),
    previous: Option<(String, Instant)>,
}

impl DpopNonces {
    pub(super) fn new(now: Instant) -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self {
            current: (URL_SAFE_NO_PAD.encode(bytes), now),
            previous: None,
        })
    }

    /// Return a challenge only for an absent or expired nonce. This operation
    /// never claims a proof identifier or changes OAuth artifact state.
    pub(super) fn check(
        &mut self,
        nonce: Option<&str>,
        now: Instant,
    ) -> Result<Option<String>, getrandom::Error> {
        if now.duration_since(self.current.1) >= Duration::from_secs(60) {
            let replacement = Self::new(now)?;
            self.previous = Some(std::mem::replace(&mut self.current, replacement.current));
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| now.duration_since(previous.1) >= Duration::from_secs(120))
        {
            self.previous = None;
        }
        let accepted = nonce.is_some_and(|nonce| {
            nonce == self.current.0
                || self
                    .previous
                    .as_ref()
                    .is_some_and(|previous| nonce == previous.0)
        });
        Ok((!accepted).then(|| self.current.0.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn parallel_tabs_share_a_nonce_and_rotation_has_a_bounded_overlap() {
        let start = Instant::now();
        let nonces = Arc::new(Mutex::new(DpopNonces::new(start).unwrap()));
        let first = nonces.lock().unwrap().check(None, start).unwrap().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&first).unwrap().len(), 32);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let nonces = &nonces;
                let first = &first;
                scope.spawn(move || {
                    assert!(nonces
                        .lock()
                        .unwrap()
                        .check(Some(first), start + Duration::from_secs(60))
                        .unwrap()
                        .is_none());
                });
            }
        });
        let mut nonces = nonces.lock().unwrap();
        let second = nonces
            .check(None, start + Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert_ne!(first, second);
        assert!(nonces
            .check(Some(&first), start + Duration::from_secs(119))
            .unwrap()
            .is_none());
        let third = nonces
            .check(Some(&first), start + Duration::from_secs(120))
            .unwrap()
            .unwrap();
        assert_ne!(third, second);
        assert!(nonces
            .check(Some(&second), start + Duration::from_secs(120))
            .unwrap()
            .is_none());
        assert!(nonces
            .check(Some(&third), start + Duration::from_secs(240))
            .unwrap()
            .is_some());
        assert!(nonces
            .check(Some(&second), start + Duration::from_secs(240))
            .unwrap()
            .is_some());
    }

    #[test]
    fn late_rotation_does_not_extend_a_previous_nonce() {
        let start = Instant::now();
        let mut nonces = DpopNonces::new(start).unwrap();
        let first = nonces.check(None, start).unwrap().unwrap();
        let second = nonces
            .check(None, start + Duration::from_secs(119))
            .unwrap()
            .unwrap();
        assert!(nonces
            .check(Some(&first), start + Duration::from_secs(119))
            .unwrap()
            .is_none());
        assert_eq!(
            nonces
                .check(Some(&first), start + Duration::from_secs(120))
                .unwrap(),
            Some(second.clone())
        );
        assert!(nonces
            .check(Some(&second), start + Duration::from_secs(120))
            .unwrap()
            .is_none());
    }
}
