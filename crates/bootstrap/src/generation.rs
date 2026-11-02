use std::fs;
use std::path::{Path, PathBuf};

use crate::constants::DEFAULT_TRELLIS_NAME;
use crate::error::BootstrapError;
use crate::nats_config::{render_nats_config, resolved_server_name, NatsTlsFiles, NatsWebsocket};
use crate::nats_material::{generate_nats_material, render_nats_accounts, NatsMaterial};
use crate::output::{create_layout, write_nats_material, write_private_file};
use crate::runtime_config::render_trellis_config;
use crate::tls::{
    generate_installation_tls, CertificateHost, GeneratedTlsSummary, BUNDLE_CA_CERTIFICATE,
    NATS_CERTIFICATE, NATS_PRIVATE_KEY, RUNTIME_CERTIFICATE, RUNTIME_PRIVATE_KEY,
};
use crate::types::{BootstrapTls, NatsBootstrapOptions, TrellisBootstrapOptions};
use crate::validate::{
    validate_nats_listener_ports, validate_output_dir, validate_required_nats_names,
    validate_required_trellis_options, validate_trellis_listener_collision,
};

/// Generate the NATS bootstrap output directory.
pub fn generate_nats_bootstrap(options: &NatsBootstrapOptions) -> Result<(), BootstrapError> {
    validate_required_nats_names(&options.config.names)?;
    validate_nats_listener_ports(&options.config)?;
    prepare_output_dir(&options.out, options.force)?;

    create_layout(&options.out)?;
    let material = generate_nats_material(&options.config.names)?;
    write_nats_conf(options, DEFAULT_TRELLIS_NAME, None, &material)?;
    write_nats_material(&options.out, &material)?;
    Ok(())
}

fn write_nats_conf(
    options: &NatsBootstrapOptions,
    trellis_name: &str,
    tls: Option<&NatsTlsFiles>,
    material: &NatsMaterial,
) -> Result<(), BootstrapError> {
    fs::write(
        options.out.join("nats.conf"),
        render_nats_config(
            &resolved_server_name(&options.config.names, trellis_name),
            options.config.nats_port,
            options.config.monitor_port,
            options.config.websocket_port,
            tls,
            &render_nats_accounts(material),
        ),
    )?;
    Ok(())
}

/// A fresh canonical base64url 32-byte key.
fn generate_key() -> String {
    use base64::Engine as _;
    let key: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
}

/// Generate a complete Trellis bootstrap bundle.
///
/// Returns the installation CA identity when Trellis generated the TLS
/// material, so callers can show users what to verify before trusting it.
pub fn generate_trellis_bootstrap(
    options: &TrellisBootstrapOptions,
) -> Result<Option<GeneratedTlsSummary>, BootstrapError> {
    validate_required_trellis_options(options)?;
    validate_nats_listener_ports(&options.nats)?;
    validate_trellis_listener_collision(options)?;
    crate::runtime_config::trellis_runtime_config(options)
        .validate_listeners()
        .map_err(|error| BootstrapError::InvalidTls(error.to_string()))?;
    validate_nats_urls(options)?;
    let ca_directory = match &options.runtime.tls {
        BootstrapTls::Generated { ca_directory } => Some(
            ca_directory
                .clone()
                .unwrap_or_else(|| default_ca_directory(&options.out)),
        ),
        _ => None,
    };
    if let Some(ca_directory) = &ca_directory {
        // The CA must never be wiped with the bundle or mixed into the files
        // roles read; inside the bundle it may only use its own folder.
        let (bundle, ca) = (resolve_path(&options.out)?, resolve_path(ca_directory)?);
        if bundle.starts_with(&ca)
            || (ca.starts_with(&bundle) && ca != bundle.join(BUNDLE_CA_DIRECTORY))
        {
            return Err(BootstrapError::InvalidTls(format!(
                "CA directory '{}' must not overlap the bundle '{}'",
                ca_directory.display(),
                options.out.display()
            )));
        }
    }
    if let BootstrapTls::Supplied(supplied) = &options.runtime.tls {
        if options.runtime.trusted_proxies.is_empty()
            && (supplied.certificate_chain.is_none() || supplied.private_key.is_none())
        {
            return Err(BootstrapError::InvalidTls(
                "supplied TLS needs the Trellis certificate chain and private key unless a \
                trusted proxy serves HTTPS"
                    .into(),
            ));
        }
        for path in [
            supplied.certificate_chain.as_ref(),
            supplied.private_key.as_ref(),
            Some(&supplied.nats_certificate_chain),
            Some(&supplied.nats_private_key),
            supplied.ca_certificate.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !path.is_file() {
                return Err(BootstrapError::InvalidTls(format!(
                    "supplied TLS file '{}' does not exist",
                    path.display()
                )));
            }
        }
    }
    prepare_output_dir(&options.out, options.force)?;
    if let Some(ca_directory) = &ca_directory {
        prepare_output_dir(ca_directory, options.force)?;
    }

    let trellis_out = &options.out;
    let mut nats_options = NatsBootstrapOptions::new(trellis_out);
    nats_options.force = false;
    nats_options.config = options.nats.clone();

    let nats_tls =
        (options.runtime.tls != BootstrapTls::LoopbackDevelopment).then(|| NatsTlsFiles {
            certificate: "/etc/nats/tls/nats.crt".into(),
            private_key: "/etc/nats/tls/nats.key".into(),
            websocket: if options.runtime.nats_websocket_via_runtime {
                NatsWebsocket::Relayed
            } else if options.runtime.nats_websocket_behind_proxy {
                NatsWebsocket::BehindProxy
            } else {
                NatsWebsocket::Tls
            },
        });
    fs::create_dir_all(trellis_out)?;
    let material = generate_nats_material(&nats_options.config.names)?;
    write_nats_conf(
        &nats_options,
        &options.runtime.name,
        nats_tls.as_ref(),
        &material,
    )?;
    let summary = match &options.runtime.tls {
        BootstrapTls::Generated { .. } => {
            let hosts = |urls: &[&str]| {
                urls.iter()
                    .map(|url| CertificateHost::from_url(url))
                    .collect::<Result<Vec<_>, _>>()
            };
            let runtime_hosts = hosts(
                &std::iter::once(options.runtime.public_origin.as_str())
                    .chain(options.runtime.console_origin.as_deref())
                    .collect::<Vec<_>>(),
            )?;
            // Managed brokers are reached over loopback by the co-located runtime.
            let mut nats_hosts = if options.runtime.nats_websocket_via_runtime {
                hosts(&[&options.runtime.nats_server_url])?
            } else {
                hosts(&[
                    &options.runtime.nats_server_url,
                    &options.runtime.nats_websocket_url,
                ])?
            };
            nats_hosts.push(CertificateHost::Dns("localhost".into()));
            nats_hosts.push(CertificateHost::Ip(std::net::Ipv4Addr::LOCALHOST.into()));
            nats_hosts.sort();
            nats_hosts.dedup();
            let mut runtime_hosts = runtime_hosts;
            runtime_hosts.sort();
            runtime_hosts.dedup();
            Some(generate_installation_tls(
                trellis_out,
                ca_directory.as_deref().unwrap_or(trellis_out),
                &format!(
                    "{} installation CA {}",
                    options.runtime.name, options.runtime.instance_id
                ),
                &runtime_hosts,
                &nats_hosts,
            )?)
        }
        BootstrapTls::Supplied(supplied) => {
            let directory = trellis_out.join("tls");
            fs::create_dir_all(&directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
            }
            for (source, target, private) in [
                (
                    supplied.certificate_chain.as_ref(),
                    RUNTIME_CERTIFICATE,
                    false,
                ),
                (supplied.private_key.as_ref(), RUNTIME_PRIVATE_KEY, true),
                (
                    Some(&supplied.nats_certificate_chain),
                    NATS_CERTIFICATE,
                    false,
                ),
                (Some(&supplied.nats_private_key), NATS_PRIVATE_KEY, true),
            ] {
                let Some(source) = source else { continue };
                let contents = fs::read(source)?;
                if private {
                    write_private_file(trellis_out.join(target), contents)?;
                } else {
                    fs::write(trellis_out.join(target), contents)?;
                }
            }
            if let Some(ca) = &supplied.ca_certificate {
                fs::write(trellis_out.join(BUNDLE_CA_CERTIFICATE), fs::read(ca)?)?;
            }
            None
        }
        BootstrapTls::LoopbackDevelopment => None,
    };
    fs::write(
        trellis_out.join("config.toml"),
        render_trellis_config(options),
    )?;
    let user = |jwt: &str, seed: &str| {
        Some(trellis_runtime::NatsUser {
            jwt: jwt.to_owned(),
            seed: seed.to_owned(),
        })
    };
    let secrets = trellis_runtime::Secrets {
        oauth_storage_key: Some(generate_key()),
        authorization_issuer_seed: Some(generate_key()),
        live_providers: trellis_runtime::LiveProviderSeeds {
            platform: Some(generate_key()),
            health: Some(generate_key()),
            jobs: Some(generate_key()),
            events: Some(generate_key()),
        },
        nats: trellis_runtime::NatsSecrets {
            users: trellis_runtime::NatsUsers {
                system: user(&material.system_user_jwt, &material.system_user_seed),
                auth: user(&material.auth_user_jwt, &material.auth_user_seed),
                trellis: user(&material.trellis_user_jwt, &material.trellis_user_seed),
            },
            auth_callout: trellis_runtime::AuthCalloutSecrets {
                issuer_signing_seed: Some(material.auth_issuer_signing_seed.clone()),
                target_signing_seed: Some(material.auth_target_signing_seed.clone()),
                xkey_seed: Some(material.auth_callout_xkey_seed.clone()),
                sentinel_public_key: Some(material.metadata.auth_sentinel_public_key.clone()),
            },
        },
    };
    write_private_file(
        trellis_out.join(trellis_runtime::SECRETS_FILE),
        secrets.to_toml(),
    )?;
    Ok(summary)
}

/// The CA folder inside a bundle, for an installation that is never mounted
/// into a role's container.
pub const BUNDLE_CA_DIRECTORY: &str = "tls-ca";

/// Where a bundle's CA lives: its own `tls-ca` folder when it has one, else
/// the `<bundle>-tls-ca` folder beside it.
pub fn bundle_ca_directory(bundle: &Path) -> PathBuf {
    let inside = bundle.join(BUNDLE_CA_DIRECTORY);
    if inside.is_dir() {
        inside
    } else {
        default_ca_directory(bundle)
    }
}

/// The default CA directory sits beside the bundle so no role mounts it.
fn default_ca_directory(out: &Path) -> PathBuf {
    let mut name = out
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| "trellis".into());
    name.push("-tls-ca");
    out.with_file_name(name)
}

/// Resolves `path` to an absolute path with every existing symlink and `..`
/// resolved, including paths whose trailing directories do not exist yet.
fn resolve_path(path: &Path) -> Result<PathBuf, BootstrapError> {
    let mut resolved = std::env::current_dir()?;
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) => resolved = PathBuf::from(component.as_os_str()),
            std::path::Component::RootDir => resolved.push(component),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(name) => {
                resolved.push(name);
                if fs::symlink_metadata(&resolved).is_ok() {
                    resolved = fs::canonicalize(&resolved)?;
                }
            }
        }
    }
    Ok(resolved)
}

/// Native NATS terminates its own TLS whenever the bundle uses TLS; only
/// loopback development may use plaintext broker endpoints.
fn validate_nats_urls(options: &TrellisBootstrapOptions) -> Result<(), BootstrapError> {
    let loopback = options.runtime.tls == BootstrapTls::LoopbackDevelopment;
    let mut urls = vec![(&options.runtime.nats_server_url, "tls", "nats")];
    if !options.runtime.nats_websocket_via_runtime {
        urls.push((&options.runtime.nats_websocket_url, "wss", "ws"));
    }
    for (url, secure, plaintext) in urls {
        let parsed = url::Url::parse(url)
            .map_err(|_| BootstrapError::InvalidTls(format!("'{url}' is not an absolute URL")))?;
        let expected = if loopback { plaintext } else { secure };
        if parsed.scheme() != expected {
            return Err(BootstrapError::InvalidTls(format!(
                "NATS endpoint '{url}' must use {expected}:// in this TLS mode"
            )));
        }
    }
    Ok(())
}

fn prepare_output_dir(out: &Path, force: bool) -> Result<(), BootstrapError> {
    validate_output_dir(out, force)?;
    if out.exists() && force {
        fs::remove_dir_all(out)?;
    }
    Ok(())
}
