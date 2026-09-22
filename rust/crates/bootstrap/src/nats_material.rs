use indexmap::IndexSet;
use nats_jwt_rs::account::{Account, ExternalAuthorization, JetStreamLimits, OperatorLimits};
use nats_jwt_rs::operator::Operator;
use nats_jwt_rs::types::{Permission, Permissions, SigningKey};
use nats_jwt_rs::user::User;
use nats_jwt_rs::Claims;
use nkeys::{KeyPair, XKey};
use serde::Serialize;

use crate::error::BootstrapError;
use crate::types::{GeneratedMetadata, NatsBootstrapNames};

#[derive(Debug)]
pub(crate) struct NatsMaterial {
    pub(crate) metadata: GeneratedMetadata,
    pub(crate) operator_jwt: String,
    pub(crate) system_account_jwt: String,
    pub(crate) auth_account_jwt: String,
    pub(crate) trellis_account_jwt: String,
    pub(crate) system_user_jwt: String,
    pub(crate) auth_user_jwt: String,
    pub(crate) trellis_user_jwt: String,
    pub(crate) system_user_seed: String,
    pub(crate) auth_user_seed: String,
    pub(crate) trellis_user_seed: String,
    pub(crate) auth_issuer_signing_seed: String,
    pub(crate) auth_target_signing_seed: String,
    pub(crate) auth_callout_xkey_seed: String,
}

pub(crate) fn generate_nats_material(
    names: &NatsBootstrapNames,
) -> Result<NatsMaterial, BootstrapError> {
    let operator_key = KeyPair::new_operator();
    let system_account_key = KeyPair::new_account();
    let auth_account_key = KeyPair::new_account();
    let trellis_account_key = KeyPair::new_account();
    let auth_signing_key = KeyPair::new_account();
    let trellis_signing_key = KeyPair::new_account();
    let system_user_key = KeyPair::new_user();
    let auth_user_key = KeyPair::new_user();
    let trellis_user_key = KeyPair::new_user();
    let auth_callout_xkey = XKey::new();

    let system_acct_pk = system_account_key.public_key();
    let auth_acct_pk = auth_account_key.public_key();
    let trellis_acct_pk = trellis_account_key.public_key();
    let auth_user_pk = auth_user_key.public_key();
    let trellis_user_pk = trellis_user_key.public_key();
    let system_user_pk = system_user_key.public_key();

    let allow_all = Permission {
        allow: vec![">".to_string()],
        deny: Vec::new(),
    };
    let allow_all_permissions = Permissions {
        publish: allow_all.clone(),
        subscribe: allow_all,
        resp: None,
    };
    let mut operator = Operator::new_claims(names.operator_name.clone(), operator_key.public_key());
    operator.payload_mut().system_account = Some(system_acct_pk.clone());
    let operator_jwt = encode_claims(&operator, &operator_key)?;

    let system_account_jwt = encode_claims(
        &Account::new_claims(names.system_account.clone(), system_acct_pk.clone()),
        &operator_key,
    )?;
    let auth_account_jwt = encode_claims(
        &account_claim(
            names.auth_account.clone(),
            auth_acct_pk.clone(),
            auth_signing_key.public_key(),
            Some(ExternalAuthorization {
                auth_users: Some([auth_user_pk.clone()].into()),
                allowed_accounts: Some([trellis_acct_pk.clone()].into()),
                xkey: Some(auth_callout_xkey.public_key()),
            }),
        ),
        &operator_key,
    )?;
    let trellis_account_jwt = encode_claims(
        &account_claim(
            names.trellis_account.clone(),
            trellis_acct_pk.clone(),
            trellis_signing_key.public_key(),
            None,
        ),
        &operator_key,
    )?;

    let system_user_jwt = encode_claims(
        &user_claim(
            "system",
            system_user_pk.clone(),
            system_acct_pk.clone(),
            allow_all_permissions.clone(),
        ),
        &system_account_key,
    )?;
    let auth_user_jwt = encode_claims(
        &user_claim(
            "auth",
            auth_user_pk.clone(),
            auth_acct_pk.clone(),
            allow_all_permissions.clone(),
        ),
        &auth_signing_key,
    )?;
    let trellis_user_jwt = encode_claims(
        &user_claim(
            "trellis",
            trellis_user_pk.clone(),
            trellis_acct_pk.clone(),
            allow_all_permissions,
        ),
        &trellis_signing_key,
    )?;
    Ok(NatsMaterial {
        metadata: GeneratedMetadata {
            system_account_name: names.system_account.clone(),
            system_account_public_key: system_acct_pk,
            system_user_public_key: system_user_pk,
            auth_account_name: names.auth_account.clone(),
            auth_account_public_key: auth_acct_pk,
            trellis_account_name: names.trellis_account.clone(),
            trellis_account_public_key: trellis_acct_pk,
            auth_user_public_key: auth_user_pk,
            trellis_user_public_key: trellis_user_pk,
        },
        operator_jwt,
        system_account_jwt,
        auth_account_jwt,
        trellis_account_jwt,
        system_user_jwt,
        auth_user_jwt,
        trellis_user_jwt,
        system_user_seed: system_user_key.seed()?,
        auth_user_seed: auth_user_key.seed()?,
        trellis_user_seed: trellis_user_key.seed()?,
        auth_issuer_signing_seed: auth_signing_key.seed()?,
        auth_target_signing_seed: trellis_signing_key.seed()?,
        auth_callout_xkey_seed: auth_callout_xkey.seed()?,
    })
}

fn account_claim(
    name: String,
    public_key: String,
    signing_public_key: String,
    authorization: Option<ExternalAuthorization>,
) -> Claims<Account> {
    let mut claim = Account::new_claims(name, public_key);
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
    account_public_key: String,
    permissions: Permissions,
) -> Claims<User> {
    let mut claim = User::new_claims(name.to_string(), public_key);
    let user = claim.payload_mut();
    user.issuer_account = Some(account_public_key);
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

pub(crate) fn render_jwt_config(material: &NatsMaterial) -> String {
    format!(
        r#"operator: {operator_jwt}
system_account: {system_account}

resolver: {{
  type: full
  dir: /data/jwt
}}

resolver_preload: {{
  {system_account}: {system_jwt}
  {auth_account}: {auth_jwt}
  {trellis_account}: {trellis_jwt}
}}
"#,
        operator_jwt = material.operator_jwt,
        system_account = material.metadata.system_account_public_key,
        system_jwt = material.system_account_jwt,
        auth_account = material.metadata.auth_account_public_key,
        auth_jwt = material.auth_account_jwt,
        trellis_account = material.metadata.trellis_account_public_key,
        trellis_jwt = material.trellis_account_jwt,
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
