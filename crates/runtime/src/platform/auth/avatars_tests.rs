use super::*;
use crate::platform::auth::{
    oauth::{OAuthEngine, OAuthStorage},
    phase_two_tests::{mutation, open},
    rpc_users,
    sqlite::id,
};
use ed25519_dalek::SigningKey;
use serde_json::json;
use std::future::IntoFuture as _;
use trellis_runtime_apis::{AuthUserCreateRequest, AuthUserProfileUpdateRequest};

#[test]
fn bounded_image_decode_reencodes_static_pixels_and_rejects_unsafe_inputs() {
    for format in [
        image::ImageFormat::Png,
        image::ImageFormat::Jpeg,
        image::ImageFormat::WebP,
    ] {
        let mut original = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            512,
            256,
            image::Rgb([23, 81, 142]),
        ))
        .write_to(&mut original, format)
        .unwrap();
        let mime = match format {
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Jpeg => "image/jpeg",
            _ => "image/webp",
        };
        let result = sanitize(mime, original.get_ref()).unwrap();
        assert!(result.len() <= 128 * 1024);
        let decoded = image::load_from_memory(&result).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (128, 64));
        assert!(sanitize("image/svg+xml", original.get_ref()).is_err());
        if format != image::ImageFormat::Png {
            assert!(sanitize("image/png", original.get_ref()).is_err());
        }
        assert!(sanitize("image/png", b"<html>not a photograph</html>").is_err());
    }
    let mut oversized = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2049, 1)
        .write_to(&mut oversized, image::ImageFormat::Png)
        .unwrap();
    assert!(sanitize("image/png", oversized.get_ref()).is_err());
    assert!(sanitize("image/jpeg", &vec![0; 256 * 1024 + 1]).is_err());
}

#[tokio::test]
async fn cached_image_delivery_is_bound_to_profile_and_installation_without_remote_browser_requests(
) {
    let directory = tempfile::tempdir().unwrap();
    let mut now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut store = open(&directory, SigningKey::from_bytes(&[37; 32]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    store.settings.oauth_origin = origin.clone();
    store.initialize_issuer(now).unwrap();
    let principal = store
        .initialize_administrator("admin", None, None, None, now)
        .unwrap()
        .principal_id;
    let store = Arc::new(store);
    let storage = OAuthStorage::new(store.clone(), &[19; 32]).unwrap();
    let engine = Arc::new(OAuthEngine::new(storage).unwrap());
    let avatars = Avatars::new(store.clone()).unwrap();
    let create: AuthUserCreateRequest = serde_json::from_value(
        json!({"username":"photo-owner","displayName":"Photo Owner","idempotencyKey":id()}),
    )
    .unwrap();
    let user = rpc_users::create_user(&store, &mutation(&principal, &create), &create, now)
        .unwrap()
        .user;
    let owner = user.principal_id.to_string();
    let provider = id();
    let registration = crate::platform::auth::lifecycle::OidcProviderRegistration {
        provider_id: provider.clone(),
        issuer: "https://identity.example".into(),
        display_name: "Identity".into(),
        client_id: "trellis".into(),
        sealed_client_secret: None,
        config: json!({"scopes":["openid","profile","email"]}),
        claim_freshness_seconds: 3600,
        expected_revision: 0,
    };
    store.transaction(|connection| {
        store.register_oidc_provider_in_transaction(connection,&mutation(&principal,&registration),&registration,now)?;
        SqliteAuthorizationStore::link_oidc_identity_in(connection,&owner,&provider,"subject",now)?;
        profiles::ingest(connection,&owner,&provider,"subject",&profiles::VerifiedProfile::from_claims(&json!({"picture":"https://images.example.com/first.png","email":"verified@example.com","email_verified":true})),now)?;
        Ok(())
    }).unwrap();
    let user = store
        .transaction(|connection| rpc_users::read_user(&store, connection, &owner, true))
        .unwrap();
    let profile = store
        .transaction(|connection| profiles::read(connection, &owner))
        .unwrap();
    assert_eq!(
        sources(&profile).len(),
        2,
        "verified OIDC picture precedes the verified-email fallback"
    );
    let url = user.avatar_url.unwrap();
    now = time::OffsetDateTime::now_utc().unix_timestamp();
    let encoded = url.rsplit('/').next().unwrap();
    let token = store
        .avatar_signer
        .get()
        .unwrap()
        .verify(encoded, now)
        .unwrap();
    let source = sources(&profile).remove(0);
    let fallback = sources(&profile).remove(1);
    avatars.save(&token, &source, Err(()), now).await.unwrap();
    avatars
        .save(&token, &fallback, Ok(None), now)
        .await
        .unwrap();
    assert!(
        matches!(avatars.get(encoded, now).await, Err(AuthError::NotFound)),
        "negative caches do not trigger another outbound fetch"
    );
    assert!(avatars
        .cached(&token, &source, now + 901)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        avatars.cached(&token, &fallback, now + 901).await.unwrap(),
        Some(None)
    ));
    let mut original = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        256,
        256,
        image::Rgb([12, 93, 143]),
    ))
    .write_to(&mut original, image::ImageFormat::Png)
    .unwrap();
    let permit = avatars.capacity.clone().acquire_owned().await.unwrap();
    drop(
        avatars
            .ingest(
                &token,
                &source,
                "image/png".into(),
                original.into_inner(),
                permit,
                now,
            )
            .await
            .unwrap(),
    );
    let shutdown = tokio_util::sync::CancellationToken::new();
    let stop = shutdown.clone();
    let server = tokio::spawn(
        axum::serve(
            listener,
            super::super::oauth::router(engine.clone(), vec![]),
        )
        .with_graceful_shutdown(async move { stop.cancelled().await })
        .into_future(),
    );
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = http.get(&url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let bytes = response.bytes().await.unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (128, 128));
    assert_eq!(
        avatars.get(encoded, now + 1).await.unwrap().0,
        bytes,
        "the retained sanitized success is reused"
    );
    let reopened = Arc::new(open(&directory, SigningKey::from_bytes(&[37; 32])));
    let retained_custody = OAuthStorage::new(reopened.clone(), &[19; 32]).unwrap();
    assert_eq!(
        Avatars::new(reopened.clone())
            .unwrap()
            .get(encoded, now + 1)
            .await
            .unwrap()
            .0,
        bytes,
        "cached pixels and the bearer survive reopening with retained custody"
    );
    drop(retained_custody);
    drop(reopened);
    let mut disabled = open(&directory, SigningKey::from_bytes(&[37; 32]));
    disabled.settings.avatar_external_fetch_enabled = false;
    let disabled = Arc::new(disabled);
    let disabled_custody = OAuthStorage::new(disabled.clone(), &[19; 32]).unwrap();
    assert!(
        matches!(
            Avatars::new(disabled.clone())
                .unwrap()
                .get(encoded, now)
                .await,
            Err(AuthError::NotFound)
        ),
        "operator disable fences already cached remote images"
    );
    assert!(disabled
        .transaction(|connection| rpc_users::read_user(&disabled, connection, &owner, true))
        .unwrap()
        .avatar_url
        .is_none());
    drop(disabled_custody);
    drop(disabled);
    assert!(store
        .avatar_signer
        .get()
        .unwrap()
        .verify(encoded, now + 600)
        .is_none());
    assert!(AvatarSigner::new(&[20; 32], &store.settings.instance_id)
        .verify(encoded, now)
        .is_none());
    assert!(AvatarSigner::new(&[19; 32], "another-installation")
        .verify(encoded, now)
        .is_none());
    let mut tampered = encoded.as_bytes().to_vec();
    tampered[10] = if tampered[10] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        http.get(format!(
            "{origin}/profile/avatars/{}",
            String::from_utf8(tampered).unwrap()
        ))
        .send()
        .await
        .unwrap()
        .status(),
        404
    );
    // The production decoder/storage populated the cache; no direct SQL image seeding.
    let application = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app_origin = format!("http://{}", application.local_addr().unwrap());
    let html = format!("<img alt='Profile photo' referrerpolicy='no-referrer' src='{url}'>");
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(move || {
            let html = html.clone();
            async move { axum::response::Html(html) }
        }),
    );
    let app_stop = shutdown.clone();
    let app_server = tokio::spawn(
        axum::serve(application, app)
            .with_graceful_shutdown(async move { app_stop.cancelled().await })
            .into_future(),
    );
    let output = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "ts/browser/profile_avatar.browser_driver.ts",
        ])
        .env("TRELLIS_TEST_AVATAR_APPLICATION", &app_origin)
        .env("TRELLIS_TEST_AVATAR_ORIGIN", &origin)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let changed:AuthUserProfileUpdateRequest=serde_json::from_value(json!({"avatarOverrideUrl":"https://images.example.com/new.png","expectedRevision":user.account.unwrap().profile_revision,"idempotencyKey":id()})).unwrap();
    rpc_users::update_profile(&store, &mutation(&owner, &changed), &changed, now + 1).unwrap();
    assert_eq!(
        http.get(&url).send().await.unwrap().status(),
        404,
        "profile changes invalidate earlier image bearers"
    );
    shutdown.cancel();
    server.await.unwrap().unwrap();
    app_server.await.unwrap().unwrap();
    drop(engine);
    drop(store);
    let store = open(&directory, SigningKey::from_bytes(&[37; 32]));
    assert_eq!(
        store
            .transaction(|connection| profiles::read(connection, &owner))
            .unwrap()
            .avatar_override
            .as_deref(),
        Some("https://images.example.com/new.png")
    );
}

#[test]
fn verified_email_sources_use_official_gravatar_hash_and_opt_out() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[37; 32]));
    store.initialize_issuer(1).unwrap();
    let principal = store
        .initialize_administrator("admin", None, None, None, 1)
        .unwrap()
        .principal_id;
    let mut profile = store
        .transaction(|connection| profiles::read(connection, &principal))
        .unwrap();
    profile.email = Some(" MyEmailAddress@example.com ".into());
    profile.email_verified = true;
    assert_eq!(sources(&profile)[0].url,"https://gravatar.com/avatar/84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee?s=128&d=404");
    profile.gravatar_enabled = false;
    assert!(sources(&profile).is_empty());
    profile.gravatar_enabled = true;
    profile.email_verified = false;
    assert!(sources(&profile).is_empty());
}
