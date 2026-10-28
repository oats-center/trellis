use super::*;

#[test]
fn oauth_registration_rejects_portal_and_remote_plaintext_callbacks_without_rebinding_clients() {
    let f = fixture();
    let before = issue_authority(&f.store, &f.admission, None, NOW).unwrap();
    let mut client = ClientRegistration {
        client_id: "native-client".into(),
        kind: "native".into(),
        display_name: "Renamed application".into(),
        redirect_uris: vec!["http://127.0.0.1:9123/callback".into()],
        development: false,
        implied_capabilities: vec![],
        metadata: serde_json::json!({}),
        expected_revision: 1,
    };
    for redirect in [
        "https://auth.example/callback",
        "http://public.example/callback",
        "https://user:password@public.example/callback",
        "https://public.example/callback#fragment",
    ] {
        client.redirect_uris = vec![redirect.into()];
        client.development = true;
        assert!(f
            .store
            .transaction(|connection| f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &client),
                &client,
                NOW + 1
            ))
            .is_err());
    }
    client.development = false;
    client.redirect_uris = vec!["http://127.0.0.1:9123/callback".into()];
    client.kind = "browser".into();
    assert!(f
        .store
        .transaction(|connection| f.store.register_client_in_transaction(
            connection,
            &mutation(&f.admin, &client),
            &client,
            NOW + 1
        ))
        .is_err());
    client.kind = "native".into();
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &client),
                &client,
                NOW + 1,
            )
        })
        .unwrap();
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    let after = issue_authority(&f.store, &f.admission, None, NOW + 1).unwrap();
    assert_eq!(
        before.authority.unsigned.binding,
        after.authority.unsigned.binding
    );
    assert_eq!(
        before.authority.unsigned.authorization_session_id,
        after.authority.unsigned.authorization_session_id
    );
    let mut browser = client;
    browser.client_id = "development-spa".into();
    browser.kind = "browser".into();
    browser.expected_revision = 0;
    assert!(f
        .store
        .transaction(|connection| f.store.register_client_in_transaction(
            connection,
            &mutation(&f.admin, &browser),
            &browser,
            NOW + 1
        ))
        .is_err());
    browser.development = true;
    f.store
        .transaction(|connection| {
            f.store.register_client_in_transaction(
                connection,
                &mutation(&f.admin, &browser),
                &browser,
                NOW + 1,
            )
        })
        .unwrap();
}
