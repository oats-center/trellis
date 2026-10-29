//! Fresh administrator and bundled public-client initialization.

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::{
    sqlite::{digest, id},
    AuthError, ClientRegistration, Mutation, SqliteAuthorizationStore,
};

pub(crate) const CONSOLE_CLIENT_ID: &str = "trellis.console";
pub(crate) const CLI_CLIENT_ID: &str = "trellis.cli";

/// Explicitly initialized native Device Code client. No callback or loopback
/// identity conveys trust; ordinary consent applies.
pub(crate) fn cli_registration() -> ClientRegistration {
    ClientRegistration {
        client_id: CLI_CLIENT_ID.into(),
        kind: "native".into(),
        display_name: "Trellis CLI".into(),
        redirect_uris: Vec::new(),
        development: false,
        implied_capabilities: Vec::new(),
        metadata: serde_json::json!({
            "client_id": CLI_CLIENT_ID,
            "application_type": "native",
            "token_endpoint_auth_method": "none",
            "grant_types": ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"]
        }),
        expected_revision: 0,
    }
}

/// The APIs each built-in runtime participant provides, with its package evidence.
pub(super) fn builtin_package_evidence(
) -> Result<Vec<(&'static [&'static str], trellis_idl::PackageEvidence)>, AuthError> {
    use trellis_rs::generated::ParticipantDescriptor;
    use trellis_runtime_apis::participants::{
        trellis_events_runtime, trellis_health_runtime, trellis_jobs_runtime, trellis_platform,
    };
    let decode = |evidence| -> Result<_, AuthError> {
        Ok(serde_json::from_value(serde_json::to_value(evidence)?)?)
    };
    Ok(vec![
        (
            trellis_platform::Participant::IMPLEMENTED_API_IDS,
            decode(trellis_platform::Participant::package_evidence())?,
        ),
        (
            trellis_health_runtime::Participant::IMPLEMENTED_API_IDS,
            decode(trellis_health_runtime::Participant::package_evidence())?,
        ),
        (
            trellis_jobs_runtime::Participant::IMPLEMENTED_API_IDS,
            decode(trellis_jobs_runtime::Participant::package_evidence())?,
        ),
        (
            trellis_events_runtime::Participant::IMPLEMENTED_API_IDS,
            decode(trellis_events_runtime::Participant::package_evidence())?,
        ),
    ])
}

#[derive(Serialize, Deserialize)]
pub(super) struct InitializationResult {
    pub(super) principal_id: String,
    // Retain the initial Argon2 verifier with the idempotent result, never a
    // fast password digest. Replay must not reset a subsequently changed secret.
    password_hash: Option<String>,
    pub(super) setup_response: Option<Vec<u8>>,
}

pub(crate) fn console_registration(origin: &str) -> Result<ClientRegistration, AuthError> {
    let origin = url::Url::parse(origin)
        .map_err(|_| AuthError::Invalid("invalid Console application origin".into()))?;
    if origin.origin().ascii_serialization() != origin.as_str().trim_end_matches('/')
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        return Err(AuthError::Invalid(
            "Console requires an exact application origin".into(),
        ));
    }
    let loopback = match origin.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if origin.scheme() != "https" && !(origin.scheme() == "http" && loopback) {
        return Err(AuthError::Invalid(
            "Console requires HTTPS or loopback HTTP".into(),
        ));
    }
    Ok(ClientRegistration {
        client_id: CONSOLE_CLIENT_ID.into(),
        kind: "browser".into(),
        display_name: "Trellis Console".into(),
        redirect_uris: vec![origin
            .join("/login")
            .map_err(|_| AuthError::Invalid("invalid Console callback".into()))?
            .to_string()],
        development: origin.scheme() == "http" && loopback,
        implied_capabilities: Vec::new(),
        metadata: serde_json::json!({}),
        expected_revision: 0,
    })
}

impl SqliteAuthorizationStore {
    /// Whether the installation has already committed its protected administrator.
    pub(crate) fn has_administrator(&self) -> Result<bool, AuthError> {
        self.transaction(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator)",
                [],
                |row| row.get(0),
            )?)
        })
    }
    /// Explicit initialization only. Ordinary startup never registers or repairs
    /// clients; normal registration enforces both management privileges.
    pub(crate) fn seed_administrator_and_console(
        &self,
        origin: &str,
        username: &str,
        password: &str,
        now: i64,
    ) -> Result<String, AuthError> {
        let registration = console_registration(origin)?;
        let callback_origin = url::Url::parse(&registration.redirect_uris[0])
            .map_err(|_| AuthError::Denied)?
            .origin()
            .ascii_serialization();
        if self.settings.reserves_origin(&callback_origin) {
            return Err(AuthError::Invalid("reserved_oauth_origin".into()));
        }
        self.initialize_administrator(
            username,
            Some(password),
            Some(&[registration, cli_registration()]),
            None,
            now,
        )
        .map(|result| result.principal_id)
    }

    pub(super) fn initialize_administrator(
        &self,
        username: &str,
        password: Option<&str>,
        registrations: Option<&[ClientRegistration]>,
        setup_storage: Option<&super::OAuthStorage>,
        now: i64,
    ) -> Result<InitializationResult, AuthError> {
        if username.trim() != username
            || username.is_empty()
            || password.is_some_and(|password| password.len() < 12)
        {
            return Err(AuthError::Invalid(
                "invalid administrator credentials".into(),
            ));
        }
        self.transaction(|connection| {
            let principal: Option<String> = connection
                .query_row(
                    "SELECT principal_id FROM auth_bootstrap_administrator WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let principal = principal.unwrap_or_else(id);
            let input = (username, registrations);
            let identity = Mutation {
                actor: principal.clone(),
                request_id: self.settings.instance_id.clone(),
                request_digest: digest(&input)?,
                session: None,
            };
            let result: InitializationResult = self.mutate_in_transaction(
                connection,
                &identity,
                "administrator.initialize",
                &input,
                now,
                |connection| {
                    Self::bootstrap_administrator_in_transaction(
                        connection,
                        &principal,
                        username,
                        password,
                        self.settings.password_min_length,
                        now,
                    )?;
                    // Use exactly the API ownership set used by production
                    // built-in provider initialization, never all accepted APIs.
                    for (apis, evidence) in builtin_package_evidence()? {
                        for api in apis {
                            let accepted: bool = connection.query_row(
                                "SELECT EXISTS(SELECT 1 FROM auth_accepted_apis WHERE api_id=?1)",
                                [api],
                                |row| row.get(0),
                            )?;
                            if !accepted {
                                Self::require_acceptance(connection, &principal, false)?;
                                let review = self.review_api_in_transaction(
                                    connection,
                                    evidence.clone(),
                                    api,
                                    now,
                                )?;
                                let acceptance = super::catalog::ApiAcceptance {
                                    review_id: review.review_id,
                                    reviewed_digest: review.definition_digest,
                                    force: false,
                                    acknowledge_breakage: false,
                                    confirm_consent_meanings: false,
                                    materially_changed: Default::default(),
                                    policy_edits: Default::default(),
                                };
                                self.accept_api_in_transaction(
                                    connection,
                                    &identity,
                                    &acceptance,
                                    now,
                                )?;
                            }
                            let mut statement = connection.prepare(
                                "SELECT capability_id \
                                FROM auth_capability_identities \
                                WHERE api_id=?1 AND state='active' AND kind='capability' \
                                ORDER BY capability_id",
                            )?;
                            let capabilities = statement
                                .query_map([api], |row| row.get::<_, String>(0))?
                                .collect::<rusqlite::Result<Vec<_>>>()?;
                            for capability_id in capabilities {
                                if capability_id == super::deployments::SELF_HEALTH_CAPABILITY
                                    || super::administration::is_administrative(&capability_id)
                                {
                                    continue;
                                }
                                super::policy::apply_policy_in_transaction(
                                    connection,
                                    &identity,
                                    &super::PolicyMutation::CapabilityGrant {
                                        principal_id: principal.clone(),
                                        capability_id,
                                        expected_revision: 0,
                                        expires_at: None,
                                    },
                                    now,
                                )?;
                            }
                        }
                    }
                    for registration in registrations.unwrap_or_default() {
                        // The Console ships with Trellis and asks only for the
                        // built-in APIs' capabilities, all allowed without asking.
                        let mut registration = registration.clone();
                        if registration.client_id == CONSOLE_CLIENT_ID {
                            for (apis, _) in builtin_package_evidence()? {
                                for api in apis {
                                    registration.implied_capabilities.extend(
                                        connection
                                            .prepare(
                                                "SELECT capability_id \
                                                FROM auth_capability_identities \
                                                WHERE api_id=?1 AND state='active' \
                                                AND kind='capability'",
                                            )?
                                            .query_map([api], |row| row.get::<_, String>(0))?
                                            .collect::<rusqlite::Result<Vec<_>>>()?,
                                    );
                                }
                            }
                            registration.implied_capabilities.sort();
                        }
                        let registration = &registration;
                        self.register_client_in_transaction(
                            connection,
                            &Mutation {
                                actor: principal.clone(),
                                request_id: id(),
                                request_digest: digest(registration)?,
                                session: None,
                            },
                            registration,
                            now,
                        )?;
                    }
                    let password_hash = connection.query_row(
                        "SELECT password_hash FROM auth_local_credentials WHERE principal_id=?1",
                        [&principal],
                        |row| row.get(0),
                    )?;
                    let setup_response = if password.is_none() {
                        match setup_storage {
                            Some(storage) => {
                                let response =
                                    super::oauth::account::create_local_reset_in_transaction(
                                        connection, &principal, now, false,
                                    )?;
                                Some(
                                    storage
                                        .encode("administrator-setup", &principal, &response)
                                        .map_err(|_| AuthError::Unavailable)?,
                                )
                            }
                            None => None,
                        }
                    } else {
                        None
                    };
                    Ok(InitializationResult {
                        principal_id: principal.clone(),
                        password_hash,
                        setup_response,
                    })
                },
            )?;
            match (password, result.password_hash.as_deref()) {
                (Some(password), Some(hash)) => {
                    let verifier = PasswordHash::new(hash).map_err(|_| AuthError::Unavailable)?;
                    Argon2::default()
                        .verify_password(password.as_bytes(), &verifier)
                        .map_err(|_| AuthError::Conflict)?;
                }
                (None, None) => {}
                _ => return Err(AuthError::Conflict),
            }
            Ok(result)
        })
    }
}

impl super::OAuthStorage {
    /// Initialize the protected administrator and registered bundled clients
    /// atomically, retaining any one-time setup response under custody encryption.
    pub(crate) fn initialize_local_administrator(
        &self,
        origin: &str,
        username: &str,
        password: Option<&str>,
        now: i64,
    ) -> Result<(String, Option<String>), AuthError> {
        let registration = console_registration(origin)?;
        let console_origin = url::Url::parse(origin)
            .map_err(|_| AuthError::Denied)?
            .origin()
            .ascii_serialization();
        if self.authority.settings.reserves_origin(&console_origin) {
            return Err(AuthError::Invalid("reserved_oauth_origin".into()));
        }
        let initialized = self.authority.initialize_administrator(
            username,
            password,
            Some(&[registration, cli_registration()]),
            Some(self),
            now,
        )?;
        let url = initialized
            .setup_response
            .as_deref()
            .map(|sealed| {
                let token: String = self
                    .decode("administrator-setup", &initialized.principal_id, sealed)
                    .map_err(|_| AuthError::Unavailable)?;
                Ok::<_, AuthError>(format!(
                    "{}/account/reset?token={token}",
                    self.authority.settings.oauth_issuer
                ))
            })
            .transpose()?;
        Ok((initialized.principal_id, url))
    }

    /// Supersede pending setup links, unless password setup is already complete.
    /// Explicit recovery may also reset an administrator with a password.
    ///
    /// A reissued setup link (`only_without_password`) stays valid until used,
    /// like the one from initialization; a recovery link lasts five minutes.
    pub(crate) fn reset_local_administrator(
        &self,
        now: i64,
        only_without_password: bool,
    ) -> Result<Option<String>, AuthError> {
        self.authority.transaction(|connection| {
            let (principal, password_hash): (String, Option<String>) = connection
                .query_row(
                    "SELECT a.principal_id, c.password_hash \
                    FROM auth_bootstrap_administrator a \
                    JOIN auth_local_credentials c ON c.principal_id=a.principal_id \
                    WHERE a.singleton=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            if only_without_password && password_hash.is_some() {
                return Ok(None);
            }
            let identity = super::Mutation {
                actor: principal.clone(),
                request_id: super::sqlite::id(),
                request_digest: super::sqlite::digest(&principal)?,
                session: None,
            };
            let sealed = self.authority.mutate_in_transaction(
                connection,
                &identity,
                "administrator.reset",
                &principal,
                now,
                |connection| {
                    let token = super::oauth::account::create_local_reset_in_transaction(
                        connection,
                        &principal,
                        now,
                        !only_without_password,
                    )?;
                    self.encode("administrator-reset", &identity.request_id, &token)
                        .map_err(|_| AuthError::Unavailable)
                },
            )?;
            let token: String = self
                .decode("administrator-reset", &identity.request_id, &sealed)
                .map_err(|_| AuthError::Unavailable)?;
            Ok(Some(format!(
                "{}/account/reset?token={token}",
                self.authority.settings.oauth_issuer
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{OAuthEngine, OAuthStorage, PolicyMutation};
    use super::*;
    use crate::{storage::SqliteStore, RuntimeConfig, SqliteStorageConfig, SubsystemName};
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use oauth_as::{client::ClientId, store::Storage};
    use std::sync::Arc;
    use trellis_runtime_apis::participants::trellis_platform;

    #[tokio::test]
    async fn seeded_clients_authenticate_and_preserve_operator_changes_on_reopen() {
        let directory = tempfile::tempdir().unwrap();
        trellis_local_bootstrap::generate_local_live_provider_seeds(
            &directory.path().join("live-providers"),
        )
        .unwrap();
        let issuer = directory.path().join("issuer.key");
        let sealing = directory.path().join("oauth.key");
        for (path, bytes) in [(&issuer, [13u8; 32]), (&sealing, [17u8; 32])] {
            std::fs::write(path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        let config: RuntimeConfig = serde_json::from_value(serde_json::json!({
            "event_session_seed_file": directory.path().join("session.seed"),
            "auth":{
                "instance_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV", "oauth_storage_key_file":sealing,
                "oauth":{"origin":"http://localhost:3001","bind_address":"127.0.0.1","port":3001,
                    "transport":{"mode":"loopback_development"}},
                "authorization":{"issuer_signing_seed_file":issuer,"context_lifetime_seconds":300,
                    "allowed_clock_skew_seconds":30}
            }
        }))
        .unwrap();
        let database = SqliteStore::new(
            SubsystemName::Platform,
            SqliteStorageConfig {
                path: directory.path().join("platform.sqlite"),
                journal_mode: Some("wal".into()),
                busy_timeout_ms: Some(5000),
                single_writer: Some(true),
            },
        );
        database.migrate().unwrap();
        let store =
            Arc::new(SqliteAuthorizationStore::open(&database, &config, "test-account").unwrap());
        store.initialize_issuer(1000).unwrap();
        assert!(matches!(
            store.seed_administrator_and_console(
                "http://localhost:3001",
                "administrator",
                "initial-password",
                1000
            ),
            Err(AuthError::Invalid(_))
        ));
        let mut invalid = cli_registration();
        invalid
            .implied_capabilities
            .push("not.installed::read".into());
        let failed = store.initialize_administrator(
            "administrator",
            Some("initial-password"),
            Some(&[
                console_registration("http://localhost:3000").unwrap(),
                invalid,
            ]),
            None,
            1000,
        );
        assert!(failed.is_err());
        let principal = store
            .seed_administrator_and_console(
                "http://localhost:3000",
                "administrator",
                "initial-password",
                1000,
            )
            .unwrap();
        let provider_seed = crate::platform::live_provider::load_live_provider_seed(
            &config,
            crate::platform::live_provider::LiveProviderRole::Platform,
        )
        .unwrap();
        let provider = store
            .initialize_builtin_provider::<trellis_platform::Participant>(&provider_seed, 1000)
            .unwrap();
        let health_grant = super::super::PolicyMutation::CapabilityGrant {
            principal_id: principal.clone(),
            capability_id: super::super::deployments::SELF_HEALTH_CAPABILITY.into(),
            expected_revision: 0,
            expires_at: None,
        };
        let grant_identity = Mutation {
            actor: principal.clone(),
            request_id: id(),
            request_digest: digest(&health_grant).unwrap(),
            session: None,
        };
        assert!(matches!(
            crate::platform::auth::tests::apply_policy(
                &store,
                &grant_identity,
                &health_grant,
                1001
            ),
            Err(AuthError::Denied)
        ));
        let cookie = store.create_portal_session(1001).unwrap();
        let login = store
            .portal_local_login(
                &cookie.cookie,
                &cookie.csrf,
                &URL_SAFE_NO_PAD.encode([23u8; 32]),
                ("administrator", "initial-password"),
                &id(),
                1001,
            )
            .unwrap();
        assert_eq!(login.principal_id, principal);
        let storage = OAuthStorage::new(store.clone(), &[17u8; 32]).unwrap();
        let client_id = ClientId::new(CONSOLE_CLIENT_ID);
        let client = storage.get_client(&client_id).await.unwrap().unwrap();
        assert_eq!(client.redirect_uris, vec!["http://localhost:3000/login"]);
        let cli_id = ClientId::new(CLI_CLIENT_ID);
        let engine =
            OAuthEngine::new(OAuthStorage::new(store.clone(), &[17u8; 32]).unwrap()).unwrap();
        let device = engine
            .server
            .device_authorization(&cli_id, None, None)
            .await
            .unwrap();
        assert!(storage
            .get_device_grant(&device.device_code)
            .await
            .unwrap()
            .is_some());
        drop(engine);
        let mut cli_change = cli_registration();
        cli_change.display_name = "Operator-managed CLI".into();
        cli_change.expected_revision = 1;
        store
            .transaction(|connection| {
                store.register_client_in_transaction(
                    connection,
                    &Mutation {
                        actor: principal.clone(),
                        request_id: id(),
                        request_digest: digest(&cli_change).unwrap(),
                        session: None,
                    },
                    &cli_change,
                    1002,
                )
            })
            .unwrap();
        let mut changed = console_registration("https://console.example").unwrap();
        changed.expected_revision = 1;
        store
            .transaction(|connection| {
                store.register_client_in_transaction(
                    connection,
                    &Mutation {
                        actor: principal.clone(),
                        request_id: id(),
                        request_digest: digest(&changed).unwrap(),
                        session: None,
                    },
                    &changed,
                    1002,
                )
            })
            .unwrap();
        drop(storage);
        drop(store);
        let store =
            Arc::new(SqliteAuthorizationStore::open(&database, &config, "test-account").unwrap());
        store.initialize_issuer(1003).unwrap();
        assert_eq!(
            store
                .seed_administrator_and_console(
                    "http://localhost:3000",
                    "administrator",
                    "initial-password",
                    1003
                )
                .unwrap(),
            principal
        );
        let reopened_seed = crate::platform::live_provider::load_live_provider_seed(
            &config,
            crate::platform::live_provider::LiveProviderRole::Platform,
        )
        .unwrap();
        let reopened_provider = store
            .initialize_builtin_provider::<trellis_platform::Participant>(&reopened_seed, 1003)
            .unwrap();
        assert_eq!(reopened_provider.identity_key_id, provider.identity_key_id);
        assert_eq!(reopened_provider.deployment_id, provider.deployment_id);
        assert_eq!(reopened_provider.instance_id, provider.instance_id);
        let storage = OAuthStorage::new(store.clone(), &[17u8; 32]).unwrap();
        assert_eq!(
            storage
                .get_client(&cli_id)
                .await
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Operator-managed CLI")
        );
        assert_eq!(
            storage
                .get_client(&client_id)
                .await
                .unwrap()
                .unwrap()
                .redirect_uris,
            vec!["https://console.example/login"]
        );
        let disable = PolicyMutation::ClientState {
            client_id: CONSOLE_CLIENT_ID.into(),
            expected_revision: 2,
            state: "disabled".into(),
        };
        crate::platform::auth::tests::apply_policy(
            &store,
            &Mutation {
                actor: principal.clone(),
                request_id: id(),
                request_digest: digest(&disable).unwrap(),
                session: None,
            },
            &disable,
            1004,
        )
        .unwrap();
        let disable_cli = PolicyMutation::ClientState {
            client_id: CLI_CLIENT_ID.into(),
            expected_revision: 2,
            state: "disabled".into(),
        };
        crate::platform::auth::tests::apply_policy(
            &store,
            &Mutation {
                actor: principal.clone(),
                request_id: id(),
                request_digest: digest(&disable_cli).unwrap(),
                session: None,
            },
            &disable_cli,
            1004,
        )
        .unwrap();
        drop(storage);
        drop(store);
        let store =
            Arc::new(SqliteAuthorizationStore::open(&database, &config, "test-account").unwrap());
        store.initialize_issuer(1005).unwrap();
        assert_eq!(
            store
                .seed_administrator_and_console(
                    "http://localhost:3000",
                    "administrator",
                    "initial-password",
                    1005
                )
                .unwrap(),
            principal
        );
        assert!(matches!(
            store.seed_administrator_and_console(
                "http://localhost:3000",
                "administrator",
                "different-password",
                1005
            ),
            Err(AuthError::Conflict)
        ));
        assert!(OAuthStorage::new(store.clone(), &[17u8; 32])
            .unwrap()
            .get_client(&client_id)
            .await
            .unwrap()
            .is_none());
        let engine =
            OAuthEngine::new(OAuthStorage::new(store.clone(), &[17u8; 32]).unwrap()).unwrap();
        assert!(engine
            .server
            .device_authorization(&cli_id, None, None)
            .await
            .is_err());
        let rotation =
            crate::platform::auth::tests::rotate_issuer(&store, &principal, 1006).unwrap();
        assert_eq!(
            store.runtime_authorization_trust().unwrap().issuer.key_id,
            rotation.next_key_id
        );
        drop(store);
        let store =
            SqliteAuthorizationStore::open_read_only(&database, &config, "test-account").unwrap();
        let trust = store.runtime_authorization_trust().unwrap();
        assert_eq!(trust.issuer.key_id, rotation.next_key_id);
        assert_eq!(trust.rotation_sequence, rotation.sequence.get());
        // Loss of committed signing custody must stop startup, not select the
        // still-present bootstrap seed or invent another issuer.
        let successor = store
            .custody_directory
            .join(format!("{}.key", rotation.next_key_id));
        let retained = successor.with_extension("retained");
        std::fs::rename(&successor, &retained).unwrap();
        assert!(SqliteAuthorizationStore::open(&database, &config, "test-account").is_err());
        assert!(
            SqliteAuthorizationStore::open_read_only(&database, &config, "test-account").is_err()
        );
        assert!(!successor.exists());
        std::fs::rename(&retained, &successor).unwrap();
        assert_eq!(
            SqliteAuthorizationStore::open(&database, &config, "test-account")
                .unwrap()
                .runtime_authorization_trust()
                .unwrap()
                .issuer,
            trust.issuer
        );
    }
}
