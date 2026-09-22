use serde_json::json;

use super::fixtures::{digest, NOW};
use crate::platform::auth::application::repository::{
    IdempotentOutcome, LoginPortalMutation, PortalRepository, PortalRouteMutation,
    PortalRouteRemoval,
};
use crate::platform::auth::{
    AuthorizationStateError, IdempotencyResultRecord, LoginPortalRecord, LoginSettingsRecord,
    PortalRouteRecord, SqliteAuthorizationStore,
};

pub(super) async fn exercise_portals(
    store: SqliteAuthorizationStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let proof = |byte: u8, purpose: &str| IdempotencyResultRecord {
        scope_key: digest(byte),
        purpose: purpose.to_owned(),
        signer_id: "signer_companion".to_owned(),
        request_id: format!("request_{byte}"),
        request_digest: digest(byte + 1),
        result: json!({ "request": byte }),
        created_at: NOW,
        expires_at: NOW + 1_000,
    };
    let portal = LoginPortalRecord {
        portal_id: "builtin".to_owned(),
        display_name: "Built-in".to_owned(),
        entry_url: None,
        builtin: true,
        disabled: false,
        removed: false,
        local_registration_enabled: true,
        provider_ids: vec!["local".to_owned()],
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    let settings = LoginSettingsRecord {
        portal_id: portal.portal_id.clone(),
        default_provider_id: Some("local".to_owned()),
        local_login_enabled: true,
        federated_registration_enabled: true,
        provider_selection_enabled: false,
        updated_at: NOW,
        version: 1,
    };
    store
        .put_login_portal(LoginPortalMutation {
            portal: portal.clone(),
            settings: settings.clone(),
            expected_version: None,
            idempotency: proof(24, "portal.put"),
            actions: Vec::new(),
        })
        .await?;
    let mut updated_portal = portal.clone();
    updated_portal.display_name = "Built-in Portal".to_owned();
    updated_portal.updated_at = NOW + 1;
    updated_portal.version = 2;
    let mut updated_settings = settings;
    updated_settings.updated_at = NOW + 1;
    updated_settings.version = 2;
    let updated_portal = match store
        .put_login_portal(LoginPortalMutation {
            portal: updated_portal,
            settings: updated_settings,
            expected_version: Some(1),
            idempotency: proof(26, "portal.put"),
            actions: Vec::new(),
        })
        .await?
    {
        IdempotentOutcome::Applied((portal, _)) => portal,
        IdempotentOutcome::Replayed(_) => return Err("unexpected portal replay".into()),
    };
    assert_eq!(updated_portal.version, 2);
    let mut removable = updated_portal.clone();
    removable.builtin = false;
    let (_, current_settings) = store
        .get_login_portal(&portal.portal_id)
        .await?
        .ok_or("missing portal")?;
    assert_eq!(
        store
            .put_login_portal(LoginPortalMutation {
                portal: removable,
                settings: current_settings,
                expected_version: Some(2),
                idempotency: proof(28, "portal.put"),
                actions: Vec::new(),
            })
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    let route = PortalRouteRecord {
        route_id: "route_1".to_owned(),
        portal_id: portal.portal_id.clone(),
        participant_id: Some("example.device".to_owned()),
        origin: None,
        deployment_id: None,
        priority: 10,
        created_at: NOW,
        updated_at: NOW,
        version: 1,
    };
    store
        .put_portal_route(PortalRouteMutation {
            route: route.clone(),
            expected_version: None,
            idempotency: proof(30, "portal-route.put"),
            actions: Vec::new(),
        })
        .await?;
    assert_eq!(store.list_portal_routes().await?, vec![route.clone()]);
    let mut duplicate = route.clone();
    duplicate.route_id = "route_2".to_owned();
    assert_eq!(
        store
            .put_portal_route(PortalRouteMutation {
                route: duplicate,
                expected_version: None,
                idempotency: proof(31, "portal-route.put"),
                actions: Vec::new(),
            })
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    let route_removal = PortalRouteRemoval {
        route_id: route.route_id.clone(),
        expected_version: 1,
        idempotency: proof(32, "portal-route.remove"),
        actions: Vec::new(),
    };
    assert_eq!(
        store.remove_portal_route(route_removal.clone()).await?,
        IdempotentOutcome::Applied(route)
    );
    assert_eq!(
        store.remove_portal_route(route_removal.clone()).await?,
        IdempotentOutcome::Replayed(route_removal.idempotency.result)
    );
    assert!(store.list_portal_routes().await?.is_empty());
    Ok(())
}
