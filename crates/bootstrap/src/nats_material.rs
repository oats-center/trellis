//! NATS accounts and users, as JWTs and nkey seeds.
//!
//! A NATS operator owns the servers. It gives Trellis its system account's
//! credentials and a signing key; Trellis creates its own two accounts with
//! that key and pushes them to the servers whenever it starts.

use indexmap::IndexSet;
use nats_jwt_rs::account::{Account, ExternalAuthorization, JetStreamLimits, OperatorLimits};
use nats_jwt_rs::operator::Operator;
use nats_jwt_rs::types::{Permission, Permissions, SigningKey};
use nats_jwt_rs::user::User;
use nats_jwt_rs::Claims;
use nkeys::{KeyPair, XKey};
use serde::Serialize;

use crate::error::BootstrapError;

/// Name of the system account an operator made here creates.
pub const SYSTEM_ACCOUNT: &str = "SYS";
/// Name of the account whose connections Trellis's auth callout decides.
pub const AUTH_ACCOUNT: &str = "AUTH";
/// Name of the account services and clients run in.
pub const TRELLIS_ACCOUNT: &str = "TRELLIS";

/// A NATS operator with a system account, as `trellis-server nats new` and
/// development installations create one.
#[derive(Clone)]
pub struct NatsOperator {
    /// Operator JWT, which every server's `nats.conf` names.
    pub jwt: String,
    /// Operator signing key seed (`SO...`) Trellis signs its accounts with.
    pub signing_seed: String,
    /// System account public key.
    pub system_account: String,
    /// System account JWT, which every server preloads.
    pub system_account_jwt: String,
    /// The system account's user.
    pub system_user: NatsUserKeys,
}

/// A NATS user's JWT and nkey seed.
#[derive(Clone)]
pub struct NatsUserKeys {
    /// User JWT signed by its account.
    pub jwt: String,
    /// User nkey seed.
    pub seed: String,
}

impl NatsUserKeys {
    /// The user as a `.creds` file, as NATS clients and the `nats` CLI read it.
    #[must_use]
    pub fn creds(&self) -> String {
        render_user_creds(&self.jwt, &self.seed)
    }
}

/// Every key of the two accounts Trellis owns on a NATS operator's servers.
#[derive(Clone)]
pub struct TrellisAccounts {
    /// Auth account seed.
    pub auth_seed: String,
    /// Trellis account seed.
    pub trellis_seed: String,
    /// Auth account signing seed; it signs callout responses.
    pub auth_signing_seed: String,
    /// Trellis account signing seed; it signs the users callouts admit.
    pub trellis_signing_seed: String,
    /// The auth account's user, which answers callouts.
    pub auth_user: NatsUserKeys,
    /// The Trellis account's user, which the runtime runs as.
    pub trellis_user: NatsUserKeys,
    /// Curve seed that decrypts callout requests.
    pub xkey_seed: String,
    /// The sentinel user clients present before a callout decides who they are.
    pub sentinel_public_key: String,
    /// The sentinel user's JWT, which Trellis publishes for clients.
    pub sentinel_jwt: String,
}

impl NatsOperator {
    /// Creates an operator, its signing key, a system account and its user.
    pub fn new(name: &str) -> Result<Self, BootstrapError> {
        let operator_key = KeyPair::new_operator();
        let signing_key = KeyPair::new_operator();
        let system_account_key = KeyPair::new_account();
        let system_user_key = KeyPair::new_user();
        let mut operator = Operator::new_claims(name.to_owned(), operator_key.public_key());
        operator.payload_mut().system_account = Some(system_account_key.public_key());
        operator.payload_mut().signing_keys = Some(vec![signing_key.public_key()]);
        let system_account_jwt = encode_claims(
            &Account::new_claims(SYSTEM_ACCOUNT.to_owned(), system_account_key.public_key()),
            &signing_key,
        )?;
        let system_user = NatsUserKeys {
            jwt: encode_claims(
                &user_claim("system", system_user_key.public_key(), None, allow_all()),
                &system_account_key,
            )?,
            seed: system_user_key.seed()?,
        };
        Ok(Self {
            jwt: encode_claims(&operator, &operator_key)?,
            signing_seed: signing_key.seed()?,
            system_account: system_account_key.public_key(),
            system_account_jwt,
            system_user,
        })
    }
}

impl TrellisAccounts {
    /// Creates the auth and Trellis accounts' keys, users and sentinel.
    pub fn new() -> Result<Self, BootstrapError> {
        let auth_account = KeyPair::new_account();
        let trellis_account = KeyPair::new_account();
        let auth_signing = KeyPair::new_account();
        let trellis_signing = KeyPair::new_account();
        let auth_user = KeyPair::new_user();
        let trellis_user = KeyPair::new_user();
        let sentinel = KeyPair::new_user();
        let deny_all = Permission {
            allow: Vec::new(),
            deny: vec![">".to_string()],
        };
        let mut sentinel_claim = user_claim(
            "trellis-auth-sentinel",
            sentinel.public_key(),
            Some(auth_account.public_key()),
            Permissions {
                publish: deny_all.clone(),
                subscribe: deny_all,
                resp: None,
            },
        );
        sentinel_claim.payload_mut().permissions.bearer_token = Some(true);
        Ok(Self {
            auth_user: NatsUserKeys {
                jwt: encode_claims(
                    &user_claim(
                        "auth",
                        auth_user.public_key(),
                        Some(auth_account.public_key()),
                        allow_all(),
                    ),
                    &auth_signing,
                )?,
                seed: auth_user.seed()?,
            },
            trellis_user: NatsUserKeys {
                jwt: encode_claims(
                    &user_claim(
                        "trellis",
                        trellis_user.public_key(),
                        Some(trellis_account.public_key()),
                        allow_all(),
                    ),
                    &trellis_signing,
                )?,
                seed: trellis_user.seed()?,
            },
            sentinel_public_key: sentinel.public_key(),
            sentinel_jwt: encode_claims(&sentinel_claim, &auth_signing)?,
            xkey_seed: XKey::new().seed()?,
            auth_seed: auth_account.seed()?,
            trellis_seed: trellis_account.seed()?,
            auth_signing_seed: auth_signing.seed()?,
            trellis_signing_seed: trellis_signing.seed()?,
        })
    }

    /// The auth and Trellis account JWTs, signed with an operator signing key,
    /// as Trellis pushes them to the servers.
    pub fn account_jwts(&self, operator_signing_seed: &str) -> Result<[String; 2], BootstrapError> {
        let signer = KeyPair::from_seed(operator_signing_seed)?;
        let public = |seed: &str| KeyPair::from_seed(seed).map(|key| key.public_key());
        let trellis = public(&self.trellis_seed)?;
        let auth_user = KeyPair::from_seed(&self.auth_user.seed)?.public_key();
        let xkey = XKey::from_seed(&self.xkey_seed)?.public_key();
        Ok([
            encode_claims(
                &account_claim(
                    AUTH_ACCOUNT,
                    public(&self.auth_seed)?,
                    public(&self.auth_signing_seed)?,
                    Some(ExternalAuthorization {
                        auth_users: Some([auth_user].into()),
                        allowed_accounts: Some([trellis.clone()].into()),
                        xkey: Some(xkey),
                    }),
                ),
                &signer,
            )?,
            encode_claims(
                &account_claim(
                    TRELLIS_ACCOUNT,
                    trellis,
                    public(&self.trellis_signing_seed)?,
                    None,
                ),
                &signer,
            )?,
        ])
    }
}

fn allow_all() -> Permissions {
    let all = Permission {
        allow: vec![">".to_string()],
        deny: Vec::new(),
    };
    Permissions {
        publish: all.clone(),
        subscribe: all,
        resp: None,
    }
}

fn account_claim(
    name: &str,
    public_key: String,
    signing_public_key: String,
    authorization: Option<ExternalAuthorization>,
) -> Claims<Account> {
    let mut claim = Account::new_claims(name.to_owned(), public_key);
    let account = claim.payload_mut();
    account.signing_keys = Some(IndexSet::from([SigningKey {
        key: signing_public_key,
        scope: None,
    }]));
    account.limits = Some(OperatorLimits {
        jetstream: Some(JetStreamLimits {
            memory_storage: Some(-1),
            disk_storage: Some(-1),
            streams: Some(-1),
            consumer: Some(-1),
            ..JetStreamLimits::default()
        }),
        ..OperatorLimits::default()
    });
    account.authorization = authorization;
    claim
}

fn user_claim(
    name: &str,
    public_key: String,
    issuer_account: Option<String>,
    permissions: Permissions,
) -> Claims<User> {
    let mut claim = User::new_claims(name.to_string(), public_key);
    let user = claim.payload_mut();
    user.issuer_account = issuer_account;
    user.permissions.permissions = permissions;
    claim
}

fn encode_claims<T>(claims: &Claims<T>, key_pair: &KeyPair) -> Result<String, BootstrapError>
where
    T: nats_jwt_rs::Claim + serde::de::DeserializeOwned + Serialize + Clone,
{
    claims
        .encode(key_pair)
        .map_err(|error| BootstrapError::Jwt(error.to_string()))
}

/// The resolver block that ends a server's `nats.conf`: the operator, its
/// system account, and a full resolver Trellis pushes its accounts to.
#[must_use]
pub fn render_nats_operator(operator: &NatsOperator, resolver_dir: &str) -> String {
    format!(
        "operator: {operator_jwt}\nsystem_account: {system_account}\n\nresolver: {{\n  \
        type: full\n  dir: {resolver_dir}\n  allow_delete: false\n}}\n\n\
        resolver_preload: {{\n  {system_account}: {system_jwt}\n}}\n",
        operator_jwt = operator.jwt,
        system_account = operator.system_account,
        system_jwt = operator.system_account_jwt,
        resolver_dir = crate::nats_config::quote_nats_path(resolver_dir),
    )
}

pub(crate) fn render_user_creds(jwt: &str, seed: &str) -> String {
    format!(
        r#"-----BEGIN NATS USER JWT-----
{jwt}
------END NATS USER JWT------

************************* IMPORTANT *************************
NKEY Seed printed below can be used sign and prove identity.
NKEYs are sensitive and should be treated as secrets.

-----BEGIN USER NKEY SEED-----
{seed}
------END USER NKEY SEED------

*************************************************************
"#
    )
}
