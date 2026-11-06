//! A NATS server folder with Trellis's accounts already in it, for running
//! NATS on its own, without a Trellis server to create them.

use std::fs;
use std::path::PathBuf;

use crate::error::BootstrapError;
use crate::files::write_private_file;
use crate::nats_material::{NatsOperator, TrellisAccounts};
use crate::types::NatsListeners;

/// Where [`generate_nats_bootstrap`] writes its folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsBootstrapOptions {
    /// Folder that receives `nats.conf` and `creds/`.
    pub out: PathBuf,
}

impl NatsBootstrapOptions {
    /// Options writing to `out`.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> Self {
        Self { out: out.into() }
    }
}

/// Writes `nats.conf` for a plaintext loopback server whose resolver holds
/// a new operator's system account and Trellis's auth and Trellis accounts,
/// plus `creds/system.creds`, `creds/auth-auth.creds` and
/// `creds/trellis-auth.creds` for their users.
///
/// # Errors
///
/// Returns [`BootstrapError`] when keys can't be made or a file written.
pub fn generate_nats_bootstrap(options: &NatsBootstrapOptions) -> Result<(), BootstrapError> {
    let operator = NatsOperator::new("Trellis")?;
    let accounts = TrellisAccounts::new()?;
    let [auth_jwt, trellis_jwt] = accounts.account_jwts(&operator.signing_seed)?;
    let public = |seed: &str| nkeys::KeyPair::from_seed(seed).map(|key| key.public_key());
    // The preload block ends the operator section; the accounts join it.
    let mut resolver = crate::nats_material::render_nats_operator(&operator, "/data/jwt");
    let end = resolver.rfind('}').unwrap_or(resolver.len());
    resolver.insert_str(
        end,
        &format!(
            "  {}: {auth_jwt}\n  {}: {trellis_jwt}\n",
            public(&accounts.auth_seed)?,
            public(&accounts.trellis_seed)?
        ),
    );
    fs::create_dir_all(options.out.join("creds"))?;
    fs::write(
        options.out.join("nats.conf"),
        crate::nats_config::render_nats_config(
            "trellis",
            NatsListeners {
                native: 4222,
                monitor: 8222,
                websocket: 8080,
            },
            None,
            crate::nats_config::NatsWebsocket::Loopback,
            false,
            &resolver,
        ),
    )?;
    for (file, user) in [
        ("system.creds", &operator.system_user),
        ("auth-auth.creds", &accounts.auth_user),
        ("trellis-auth.creds", &accounts.trellis_user),
    ] {
        write_private_file(options.out.join("creds").join(file), user.creds())?;
    }
    Ok(())
}
