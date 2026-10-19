//! Image-only short-lived bearers and bounded presentation-image storage.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use image::ImageDecoder as _;
use ring::hmac;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    profiles::{self, Profile},
    remote_http, AuthError, SqliteAuthorizationStore,
};

pub(super) struct AvatarSigner(hmac::Key);

pub(super) struct ImageToken {
    principal: String,
    revision: u64,
    expires: i64,
}

#[cfg(test)]
#[path = "avatars_tests.rs"]
mod tests;

impl AvatarSigner {
    pub(super) fn new(key: &[u8; 32], instance: &str) -> Self {
        let root = hmac::Key::new(hmac::HMAC_SHA256, key);
        let derived = hmac::sign(
            &root,
            format!("trellis.auth.profile-avatar.key.v1\0{instance}").as_bytes(),
        );
        Self(hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()))
    }

    pub(super) fn mint(&self, principal: &str, revision: u64, now: i64) -> Option<String> {
        let principal: ulid::Ulid = principal.parse().ok()?;
        let mut bytes = Vec::with_capacity(65);
        bytes.push(1);
        bytes.extend_from_slice(&principal.to_bytes());
        bytes.extend_from_slice(&revision.to_be_bytes());
        bytes.extend_from_slice(&now.checked_add(600)?.to_be_bytes());
        let mac = hmac::sign(&self.0, &bytes);
        bytes.extend_from_slice(mac.as_ref());
        Some(URL_SAFE_NO_PAD.encode(bytes))
    }

    fn verify(&self, token: &str, now: i64) -> Option<ImageToken> {
        if token.len() != 87 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
        if bytes.len() != 65
            || bytes[0] != 1
            || hmac::verify(&self.0, &bytes[..33], &bytes[33..]).is_err()
        {
            return None;
        }
        let expires = i64::from_be_bytes(bytes[25..33].try_into().ok()?);
        if expires <= now || expires > now.checked_add(600)? {
            return None;
        }
        Some(ImageToken {
            principal: ulid::Ulid::from_bytes(bytes[1..17].try_into().ok()?).to_string(),
            revision: u64::from_be_bytes(bytes[17..25].try_into().ok()?),
            expires,
        })
    }
}

#[derive(Clone)]
struct Source {
    category: &'static str,
    url: String,
    fingerprint: String,
}

fn sources(profile: &Profile) -> Vec<Source> {
    let gravatar = profile
        .email
        .as_ref()
        .filter(|_| profile.email_verified && profile.gravatar_enabled)
        .map(|email| {
            format!(
                "https://gravatar.com/avatar/{}?s=128&d=404",
                Sha256::digest(email.trim().to_lowercase().as_bytes())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )
        });
    [
        ("override", profile.avatar_override.as_ref()),
        ("oidc", profile.picture.as_ref()),
        ("gravatar", gravatar.as_ref()),
    ]
    .into_iter()
    .filter_map(|(category, url)| {
        let url = url::Url::parse(url?).ok()?;
        if !remote_http::public_url(&url) {
            return None;
        }
        let fingerprint = URL_SAFE_NO_PAD.encode(Sha256::digest(
            format!("{category}\0{}\0{url}", profile.revision).as_bytes(),
        ));
        Some(Source {
            category,
            url: url.into(),
            fingerprint,
        })
    })
    .collect()
}

/// Decode only the allowed formats within explicit allocation/dimension bounds.
/// Re-encoding emits a static PNG without source metadata or animation.
fn sanitize(mime: &str, bytes: &[u8]) -> Result<Vec<u8>, ()> {
    if bytes.len() > 256 * 1024 {
        return Err(());
    }
    let format = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => return Err(()),
    };
    if image::guess_format(bytes).map_err(|_| ())? != format {
        return Err(());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| ())?;
    let orientation = decoder.orientation().map_err(|_| ())?;
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|_| ())?;
    image.apply_orientation(orientation);
    let image = image::DynamicImage::ImageRgba8(image.thumbnail(128, 128).to_rgba8());
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|_| ())?;
    let bytes = encoded.into_inner();
    if bytes.len() > 128 * 1024 {
        return Err(());
    }
    Ok(bytes)
}

pub(super) struct Avatars {
    store: Arc<SqliteAuthorizationStore>,
    client: reqwest::Client,
    capacity: Arc<Semaphore>,
    flights: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl Avatars {
    pub(super) fn new(store: Arc<SqliteAuthorizationStore>) -> Result<Self, AuthError> {
        Ok(Self {
            store,
            client: remote_http::client()
                .timeout(Duration::from_secs(3))
                .build()
                .map_err(|_| AuthError::Unavailable)?,
            capacity: Arc::new(Semaphore::new(8)),
            flights: Mutex::new(HashMap::new()),
        })
    }

    async fn cached(
        &self,
        token: &ImageToken,
        source: &Source,
        now: i64,
    ) -> Result<Option<Option<Vec<u8>>>, AuthError> {
        let store = self.store.clone();
        let principal = token.principal.clone();
        let revision = token.revision;
        let fingerprint = source.fingerprint.clone();
        tokio::task::spawn_blocking(move || store.transaction(|connection| {
            let current:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_principals p JOIN auth_user_profiles u USING(principal_id) WHERE p.principal_id=?1 AND p.kind='user' AND p.state='active' AND u.version=?2)",params![principal,revision],|row|row.get(0))?;
            if !current {return Err(AuthError::NotFound);}
            Ok(connection.query_row("SELECT image_bytes FROM auth_avatar_cache WHERE principal_id=?1 AND source_fingerprint=?2 AND expires_at>?3",params![principal,fingerprint,now],|row|row.get::<_,Option<Vec<u8>>>(0)).optional()?)
        })).await.map_err(|_|AuthError::Unavailable)?
    }

    async fn save(
        &self,
        token: &ImageToken,
        source: &Source,
        result: Result<Option<Vec<u8>>, ()>,
        now: i64,
    ) -> Result<(), AuthError> {
        let store = self.store.clone();
        let principal = token.principal.clone();
        let revision = token.revision;
        let fingerprint = source.fingerprint.clone();
        tokio::task::spawn_blocking(move || store.transaction(|connection| {
            let current:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_principals p JOIN auth_user_profiles u USING(principal_id) WHERE p.principal_id=?1 AND p.kind='user' AND p.state='active' AND u.version=?2)",params![principal,revision],|row|row.get(0))?;
            if !current {return Err(AuthError::NotFound);}
            let (status,bytes,ttl)=match result { Ok(Some(bytes))=>("ok",Some(bytes),7*86400),Ok(None)=>("not_found",None,86400),Err(())=>("temporary_error",None,900) };
            connection.execute("INSERT INTO auth_avatar_cache(principal_id,source_fingerprint,status,mime,image_bytes,fetched_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(principal_id,source_fingerprint) DO UPDATE SET status=excluded.status,mime=excluded.mime,image_bytes=excluded.image_bytes,fetched_at=excluded.fetched_at,expires_at=excluded.expires_at",params![principal,fingerprint,status,bytes.as_ref().map(|_|"image/png"),bytes,now,now+ttl])?;
            Ok(())
        })).await.map_err(|_|AuthError::Unavailable)?
    }

    async fn ingest(
        &self,
        token: &ImageToken,
        source: &Source,
        mime: String,
        bytes: Vec<u8>,
        permit: OwnedSemaphorePermit,
        now: i64,
    ) -> Result<OwnedSemaphorePermit, AuthError> {
        // The permit remains owned by bounded decoding even if HTTP is cancelled.
        let (result, permit) =
            tokio::task::spawn_blocking(move || (sanitize(&mime, &bytes).map(Some), permit))
                .await
                .map_err(|_| AuthError::Unavailable)?;
        self.save(token, source, result, now).await?;
        Ok(permit)
    }

    async fn fetch(&self, source: &Source) -> Result<Option<(String, Vec<u8>)>, ()> {
        let url = url::Url::parse(&source.url).map_err(|_| ())?;
        if !remote_http::public_url(&url) {
            return Err(());
        }
        let mut response = self.client.get(url).send().await.map_err(|_| ())?;
        if response
            .remote_addr()
            .is_none_or(|address| !remote_http::public_address(address.ip()))
        {
            return Err(());
        }
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > 256 * 1024)
            || response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|encoding| encoding != "identity")
        {
            return Err(());
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
            return Err(());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes.len().saturating_add(chunk.len()) > 256 * 1024 {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some((mime, bytes)))
    }

    pub(super) async fn get(&self, encoded: &str, now: i64) -> Result<(Vec<u8>, i64), AuthError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        if !self.store.settings.avatar_external_fetch_enabled {
            return Err(AuthError::NotFound);
        }
        let token = self
            .store
            .avatar_signer
            .get()
            .and_then(|signer| signer.verify(encoded, now))
            .ok_or(AuthError::NotFound)?;
        let store = self.store.clone();
        let principal = token.principal.clone();
        let profile = tokio::task::spawn_blocking(move || {
            store.transaction(|connection| profiles::read(connection, &principal))
        })
        .await
        .map_err(|_| AuthError::Unavailable)??;
        if profile.revision != token.revision {
            return Err(AuthError::NotFound);
        }
        for source in sources(&profile) {
            match self.cached(&token, &source, now).await? {
                Some(Some(bytes)) => return Ok((bytes, token.expires)),
                Some(None) => continue,
                None => {}
            }
            let mut permit = self
                .capacity
                .clone()
                .try_acquire_owned()
                .map_err(|_| AuthError::Unavailable)?;
            let flight = {
                let mut flights = self.flights.lock().map_err(|_| AuthError::Unavailable)?;
                flights.retain(|_, flight| flight.strong_count() > 0);
                let key = format!("{}:{}", token.principal, source.fingerprint);
                if let Some(flight) = flights.get(&key).and_then(Weak::upgrade) {
                    flight
                } else {
                    let flight = Arc::new(tokio::sync::Mutex::new(()));
                    flights.insert(key, Arc::downgrade(&flight));
                    flight
                }
            };
            let _guard = tokio::time::timeout_at(deadline, flight.lock())
                .await
                .map_err(|_| AuthError::Unavailable)?;
            if let Some(cached) = self.cached(&token, &source, now).await? {
                if let Some(bytes) = cached {
                    return Ok((bytes, token.expires));
                }
                continue;
            }
            match tokio::time::timeout_at(deadline, self.fetch(&source)).await {
                Ok(Ok(Some((mime, bytes)))) => {
                    match tokio::time::timeout_at(
                        deadline,
                        self.ingest(&token, &source, mime, bytes, permit, now),
                    )
                    .await
                    {
                        Ok(result) => permit = result?,
                        Err(_) => {
                            self.save(&token, &source, Err(()), now).await?;
                            return Err(AuthError::NotFound);
                        }
                    }
                }
                Ok(Ok(None)) => self.save(&token, &source, Ok(None), now).await?,
                result => {
                    tracing::debug!(principal_id=%token.principal,source=source.category,reason="fetch_failed","profile image unavailable");
                    self.save(&token, &source, Err(()), now).await?;
                    if result.is_err() {
                        return Err(AuthError::NotFound);
                    }
                }
            }
            drop(permit);
            if let Some(Some(bytes)) = self.cached(&token, &source, now).await? {
                return Ok((bytes, token.expires));
            }
        }
        Err(AuthError::NotFound)
    }
}
