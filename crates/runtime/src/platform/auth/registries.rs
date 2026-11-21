//! Registries Auth pulls published packages from, and the pull itself.
//!
//! An administrator adds a registry by name with the workspace it serves, and
//! for a private one the user and secret Trellis signs in with; the secret is
//! sealed with the storage key and never returned. `Packages.Pull` fetches a
//! release from its package's own workspace, signing in with the registry
//! added for that workspace or anonymously, and accepts it only when the
//! release declares the identity it was pulled under: whoever can push to a
//! workspace owns its names. It checks the manifest, config, and source-layer
//! digests and the release's own frozen resolution, pulls every package of
//! that resolution from its own workspace by its exact manifest digest, and
//! compiles the release from exactly those packages. The evidence it returns
//! is ordinary package evidence, so `Deployments.Create` and `Apply` still
//! recompile all of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use oci_client::client::{Client, ClientConfig, ClientProtocol};
use oci_client::manifest::{OciImageManifest, OCI_IMAGE_MEDIA_TYPE};
use oci_client::secrets::RegistryAuth;
use oci_client::Reference;
use rusqlite::params;
use serde_json::json;
use trellis_idl::distribution::{
    extract_archive, frozen_resolution, package_manifest, sha256, validate_package_digest,
    PackageConfig, CONFIG_MEDIA_TYPE, MAX_ARCHIVE_BYTES, MAX_CONFIG_BYTES, MAX_MANIFEST_BYTES,
    RESOLUTION_PATH, SOURCE_MEDIA_TYPE,
};
use trellis_idl::project::LockedSource;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{__types::trellis::*, apis::trellis_auth_v1::Provider};

use super::oauth::storage::OAuthStorage;
use super::rpc::{caller, mutation, rpc_error};
use super::sqlite::{digest, timestamp, Connection};
use super::{blocking, Administration, AuthError, Mutation, SqliteAuthorizationStore};

/// The seal purpose that binds a secret to its registry's name.
const SEAL: &str = "packageRegistry";

/// One registry as Auth pulls from it.
struct Registry {
    name: String,
    prefix: String,
    auth: RegistryAuth,
}

fn read_registry(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, Option<String>, i64)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(4)?))
}

fn summary(
    (name, prefix, username, updated_at): (String, String, Option<String>, i64),
) -> Result<AuthPackageRegistry, AuthError> {
    Ok(AuthPackageRegistry {
        name: AuthShortName(name),
        prefix: AuthName(prefix),
        username: username.map(AuthName),
        updated_at: timestamp(updated_at)?,
        extra: Default::default(),
    })
}

/// Every registry with its credentials opened, for pulling.
fn registries(connection: &Connection, storage: &OAuthStorage) -> Result<Vec<Registry>, AuthError> {
    let mut statement = connection
        .prepare("SELECT name,prefix,username,sealed_secret FROM auth_package_registries")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<Vec<u8>>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(name, prefix, username, sealed)| {
            let auth = match (username, sealed) {
                (Some(username), Some(sealed)) => RegistryAuth::Basic(
                    username,
                    storage
                        .decode::<String>(SEAL, &name, &sealed)
                        .map_err(|_| AuthError::Unavailable)?,
                ),
                _ => RegistryAuth::Anonymous,
            };
            Ok(Registry { name, prefix, auth })
        })
        .collect()
}

/// A prefix is a workspace: a registry host and repository path, never a
/// URL, so it names one place packages are pulled from.
fn validate_prefix(prefix: &str) -> Result<(), AuthError> {
    let valid = prefix != trellis_protocol::LOCAL_WORKSPACE
        && trellis_protocol::validate_workspace(prefix).is_ok()
        && Reference::from_str(&format!("{prefix}/package"))
            .is_ok_and(|reference| prefix.starts_with(reference.resolve_registry()));
    if valid {
        Ok(())
    } else {
        Err(AuthError::Invalid(format!(
            "registry prefix {prefix} is not a workspace, a lower-case registry host and \
            repository path such as ghcr.io/oats-center"
        )))
    }
}

fn put(
    storage: &OAuthStorage,
    identity: &Mutation,
    input: &AuthPackageRegistryPutRequest,
    now: i64,
) -> Result<AuthPackageRegistryPutResponse, AuthError> {
    validate_prefix(&input.prefix)?;
    let name = input.name.0.clone();
    // The secret never reaches the audit record or the replay digest's preimage.
    let audit = json!({
        "requestDigest": identity.request_digest,
        "name": name,
        "prefix": input.prefix,
        "username": input.credentials.as_ref().map(|credentials| &credentials.username),
    });
    let safe = Mutation {
        request_digest: digest(&audit)?,
        ..identity.clone()
    };
    let sealed = input
        .credentials
        .as_ref()
        .map(|credentials| {
            storage
                .encode(SEAL, &name, &credentials.secret.0)
                .map_err(|_| AuthError::Unavailable)
        })
        .transpose()?;
    storage
        .authority
        .mutate(&safe, "rpc.packageRegistries.put", &audit, now, |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManagePrincipals,
            )?;
            connection.execute(
                "INSERT INTO auth_package_registries(name,prefix,username,sealed_secret,updated_at) \
                VALUES(?1,?2,?3,?4,?5) ON CONFLICT(name) DO UPDATE SET prefix=excluded.prefix, \
                username=excluded.username, sealed_secret=excluded.sealed_secret, \
                updated_at=excluded.updated_at",
                params![
                    name,
                    input.prefix.0.trim_end_matches('/'),
                    input
                        .credentials
                        .as_ref()
                        .map(|credentials| credentials.username.0.as_str()),
                    sealed,
                    now
                ],
            )?;
            let row = connection.query_row(
                "SELECT name,prefix,username,sealed_secret,updated_at \
                FROM auth_package_registries WHERE name=?1",
                [&name],
                read_registry,
            )?;
            Ok(AuthPackageRegistryPutResponse {
                registry: summary(row)?,
                extra: Default::default(),
            })
        })
}

/// Register `Auth.PackageRegistries` and `Auth.Packages.Pull`.
pub(super) fn register_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    engine: Arc<super::OAuthEngine>,
) {
    let lister = Arc::clone(&engine);
    provider.register_package_registries_list(move |context, _input| {
        let engine = Arc::clone(&lister);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            blocking(move || {
                engine.server.store().authority.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    let mut statement = connection.prepare(
                        "SELECT name,prefix,username,sealed_secret,updated_at \
                        FROM auth_package_registries ORDER BY name",
                    )?;
                    let rows = statement
                        .query_map([], read_registry)?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(AuthPackageRegistriesListResponse {
                        registries: rows.into_iter().map(summary).collect::<Result<_, _>>()?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let putter = Arc::clone(&engine);
    provider.register_package_registries_put(move |context, input| {
        let engine = Arc::clone(&putter);
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManagePrincipals),
                &input,
            )
            .map_err(rpc_error)?;
            blocking(move || {
                put(
                    engine.server.store(),
                    &identity,
                    &input,
                    crate::clock::unix_now(),
                )
            })
            .await
            .map_err(rpc_error)
        }
    });
    let remover = Arc::clone(&engine);
    provider.register_package_registries_remove(move |context, input| {
        let engine = Arc::clone(&remover);
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManagePrincipals),
                &input,
            )
            .map_err(rpc_error)?;
            blocking(move || {
                engine.server.store().authority.mutate(
                    &identity,
                    "rpc.packageRegistries.remove",
                    &input,
                    crate::clock::unix_now(),
                    |connection| {
                        SqliteAuthorizationStore::require_administration(
                            connection,
                            &identity.actor,
                            Administration::ManagePrincipals,
                        )?;
                        let removed = connection.execute(
                            "DELETE FROM auth_package_registries WHERE name=?1",
                            [input.name.0.as_str()],
                        )?;
                        if removed == 0 {
                            return Err(AuthError::NotFound);
                        }
                        Ok(AuthEmpty {
                            extra: Default::default(),
                        })
                    },
                )
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_packages_pull(move |context, input| {
        let engine = Arc::clone(&engine);
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePrincipals)).map_err(rpc_error)?;
            let opened = Arc::clone(&engine);
            let registries = blocking(move || {
                let storage = opened.server.store();
                storage.authority.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePrincipals,
                    )?;
                    registries(connection, storage)
                })
            })
            .await
            .map_err(rpc_error)?;
            pull(&registries, &input).await.map_err(rpc_error)
        }
    });
}

/// One release as pulled and checked.
struct Release {
    config: PackageConfig,
    manifest_digest: String,
    distribution_digest: String,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

fn invalid(message: impl Into<String>) -> AuthError {
    AuthError::Invalid(message.into())
}

fn client(reference: &Reference) -> Result<Client, AuthError> {
    // Reqwest needs a process provider for its own TLS setup.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let host = reference.resolve_registry();
    // Only a registry on this machine is reached over plain HTTP.
    let loopback = host.starts_with("localhost:") || host.starts_with("127.0.0.1:");
    Client::try_from(ClientConfig {
        protocol: if loopback {
            ClientProtocol::HttpsExcept(vec![host.to_owned()])
        } else {
            ClientProtocol::Https
        },
        connect_timeout: Some(Duration::from_secs(10)),
        read_timeout: Some(Duration::from_secs(30)),
        ..ClientConfig::default()
    })
    .map_err(|error| invalid(format!("couldn't reach {host}: {error}")))
}

/// Pull one release and check every digest its manifest and config name.
async fn pull_release(reference: &Reference, auth: &RegistryAuth) -> Result<Release, AuthError> {
    let client = client(reference)?;
    let (manifest_bytes, manifest_digest) = client
        .pull_manifest_raw(reference, auth, &[OCI_IMAGE_MEDIA_TYPE])
        .await
        .map_err(|error| invalid(format!("couldn't pull {reference}: {error}")))?;
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(invalid(format!("{reference} has too large a manifest")));
    }
    if sha256(&manifest_bytes) != manifest_digest
        || reference
            .digest()
            .is_some_and(|wanted| wanted != manifest_digest)
    {
        return Err(invalid(format!(
            "{reference} came back with a manifest whose digest doesn't match"
        )));
    }
    let manifest: OciImageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| invalid(format!("{reference} has an invalid manifest: {error}")))?;
    let layer_limit = i64::try_from(MAX_ARCHIVE_BYTES).unwrap_or(i64::MAX);
    if !(manifest.schema_version == 2
        && manifest.media_type.as_deref() == Some(OCI_IMAGE_MEDIA_TYPE)
        && manifest.artifact_type.as_deref() == Some(SOURCE_MEDIA_TYPE)
        && manifest.config.media_type == CONFIG_MEDIA_TYPE
        && (0..=MAX_CONFIG_BYTES).contains(&manifest.config.size)
        && manifest.layers.len() == 1
        && manifest.layers[0].media_type == SOURCE_MEDIA_TYPE
        && (0..=layer_limit).contains(&manifest.layers[0].size))
    {
        return Err(invalid(format!("{reference} is not a Trellis package")));
    }
    // Each blob is checked against the digest its descriptor names.
    let mut config_bytes = Vec::new();
    let mut layer = Vec::new();
    for (descriptor, bytes) in [
        (&manifest.config, &mut config_bytes),
        (&manifest.layers[0], &mut layer),
    ] {
        client
            .pull_blob(reference, descriptor, &mut *bytes)
            .await
            .map_err(|error| invalid(format!("couldn't pull {reference}: {error}")))?;
        if i64::try_from(bytes.len()).ok() != Some(descriptor.size)
            || sha256(bytes) != descriptor.digest
        {
            return Err(invalid(format!(
                "{reference} came back with content whose digest doesn't match its manifest"
            )));
        }
    }
    let config = PackageConfig::parse(&config_bytes)
        .map_err(|error| invalid(format!("{reference}: {error}")))?;
    validate_package_digest(&config.package_digest)
        .map_err(|error| invalid(format!("{reference}: {error}")))?;
    let files =
        extract_archive(&layer).map_err(|error| invalid(format!("{reference}: {error}")))?;
    let manifest = package_manifest(&files).map_err(|error| invalid(error.to_string()))?;
    if manifest.identity() != config.id || manifest.package.version.to_string() != config.version {
        return Err(invalid(format!(
            "{reference}'s trellis.toml names a different package than its config"
        )));
    }
    Ok(Release {
        config,
        manifest_digest,
        distribution_digest: sha256(&layer),
        files,
    })
}

fn reference(repository: &str, tag_or_digest: &str) -> Result<Reference, AuthError> {
    let separator = if tag_or_digest.starts_with("sha256:") {
        "@"
    } else {
        ":"
    };
    Reference::from_str(&format!("{repository}{separator}{tag_or_digest}"))
        .map_err(|error| invalid(format!("{repository}{separator}{tag_or_digest}: {error}")))
}

/// A release's files as Packages.Resolve takes them.
fn sources(release: &Release) -> Result<AuthPackageSources, AuthError> {
    let text = |path: &Path, bytes: &[u8]| {
        String::from_utf8(bytes.to_vec()).map_err(|_| {
            invalid(format!(
                "{} {}'s {} is not UTF-8",
                release.config.id,
                release.config.version,
                path.display()
            ))
        })
    };
    let mut manifest = None;
    let mut files = Vec::new();
    for (path, bytes) in &release.files {
        if path == Path::new("trellis.toml") {
            manifest = Some(text(path, bytes)?);
        } else if path != Path::new(RESOLUTION_PATH) {
            files.push(AuthPackageFile {
                path: AuthPath(path.to_string_lossy().into_owned()),
                text: text(path, bytes)?,
                extra: Default::default(),
            });
        }
    }
    Ok(AuthPackageSources {
        manifest,
        name: None,
        version: None,
        files,
        extra: Default::default(),
    })
}

/// How Trellis signs in to `workspace`: with the one registry added for it,
/// or anonymously when there is none. Several would leave the credentials to
/// chance.
fn workspace_auth(
    registries: &[Registry],
    workspace: &str,
    package: &str,
) -> Result<RegistryAuth, AuthError> {
    let matching = registries
        .iter()
        .filter(|registry| registry.prefix == workspace)
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [] => Ok(RegistryAuth::Anonymous),
        [only] => Ok(only.auth.clone()),
        several => Err(invalid(format!(
            "{package} is pulled from {workspace}, which registries {} all name; keep one of \
            them",
            several
                .iter()
                .map(|registry| registry.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

async fn pull(
    registries: &[Registry],
    input: &AuthPackagePullRequest,
) -> Result<AuthPackagePullResponse, AuthError> {
    let identity = input.package.0.as_str();
    let repository = trellis_idl::project::package_repository(identity)
        .map_err(|error| invalid(error.to_string()))?;
    let workspace = trellis_idl::project::package_source_workspace(identity)
        .map_err(|error| invalid(error.to_string()))?;
    let wanted = match (&input.version, &input.digest) {
        (Some(version), None) => {
            semver::Version::parse(version)
                .map_err(|error| invalid(format!("version {}: {error}", version.0)))?;
            version.0.as_str()
        }
        (None, Some(digest)) => digest.0.as_str(),
        _ => return Err(invalid("give exactly one of version and digest")),
    };
    let auth = workspace_auth(registries, &workspace, identity)?;
    let root = pull_release(&reference(&repository, wanted)?, &auth).await?;
    // The repository is the identity asked for, so a release that declares
    // another workspace or name was pushed somewhere it does not own.
    if root.config.id != identity
        || input
            .version
            .as_ref()
            .is_some_and(|version| version.0 != root.config.version)
    {
        return Err(invalid(format!(
            "{repository} holds {} {}, not the release asked for",
            root.config.id, root.config.version
        )));
    }
    let resolution = frozen_resolution(&root.files).map_err(|error| invalid(error.to_string()))?;
    // Every package the release was published with, pulled from its own
    // workspace by the exact manifest digest it was published with.
    let mut dependencies = Vec::with_capacity(resolution.packages.len());
    for locked in &resolution.packages {
        // The built-in package's authority is this server, not a registry:
        // the release compiles against the copy it ships, and the compiled
        // digests below must still equal the ones it was published with.
        if locked.id == trellis_idl::project::BUILTIN_PACKAGE {
            continue;
        }
        let (
            LockedSource::Registry {
                workspace: recorded,
                oci_digest,
            },
            Ok(expected),
        ) = (
            &locked.source,
            trellis_idl::project::package_source_workspace(&locked.id),
        )
        else {
            return Err(invalid(format!(
                "{} was published with a dependency on {} that no registry holds",
                root.config.id, locked.id
            )));
        };
        if *recorded != expected {
            return Err(invalid(format!(
                "{} records {} as pulled from {recorded}, which is not its workspace",
                root.config.id, locked.id
            )));
        }
        let repository = trellis_idl::project::package_repository(&locked.id)
            .map_err(|error| invalid(error.to_string()))?;
        let auth = workspace_auth(registries, recorded, &locked.id)?;
        let release = pull_release(&reference(&repository, oci_digest)?, &auth).await?;
        if release.config.id != locked.id
            || release.config.version != locked.version.to_string()
            || release.config.package_digest != locked.digest
            || release.distribution_digest != locked.distribution_digest
        {
            return Err(invalid(format!(
                "{repository}@{oci_digest} is not the {} {} {} was published with",
                locked.id, locked.version, root.config.id
            )));
        }
        dependencies.push(release);
    }
    let request = AuthPackageResolveRequest {
        root: sources(&root)?,
        packages: Some(dependencies.iter().map(sources).collect::<Result<_, _>>()?),
        extra: Default::default(),
    };
    let frozen = resolution
        .packages
        .iter()
        .map(|locked| (locked.id.clone(), locked.digest.clone()))
        .chain([(root.config.id.clone(), root.config.package_digest.clone())])
        .collect::<BTreeMap<_, _>>();
    let published = AuthPublishedPackage {
        package: AuthName(root.config.id.clone()),
        version: AuthShortName(root.config.version.clone()),
        digest: AuthDigest(root.manifest_digest.clone()),
        extra: Default::default(),
    };
    blocking(move || {
        // Only the release's own packages and the built-in package are
        // offered, so nothing an installed participant holds can stand in for
        // one of them.
        let resolved = super::packages::resolve(super::packages::builtin_evidence()?, &request)?;
        let problems = resolved
            .missing
            .iter()
            .map(|missing| format!("nothing provides {}", missing.name.0))
            .chain(
                resolved
                    .rejected
                    .iter()
                    .map(|rejected| format!("{}: {}", rejected.name.0, rejected.reason)),
            )
            .collect::<Vec<_>>();
        let evidence = match resolved.package_evidence {
            Some(evidence) if problems.is_empty() => evidence,
            _ => {
                return Err(invalid(format!(
                    "{} {} doesn't compile from the packages it was published with: {}",
                    published.package.0,
                    published.version.0,
                    problems.join("; ")
                )))
            }
        };
        let compiled = evidence
            .packages
            .iter()
            .map(|package| (package.name.0.clone(), package.digest.0.clone()))
            .collect::<BTreeMap<_, _>>();
        if compiled != frozen {
            return Err(invalid(format!(
                "{} {} compiles to different packages than it was published with",
                published.package.0, published.version.0
            )));
        }
        Ok(AuthPackagePullResponse {
            published,
            package_evidence: evidence,
            participants: resolved.participants,
            dependencies: resolved
                .dependencies
                .into_iter()
                .map(|dependency| AuthResolvedPackage {
                    origin: AuthPackageOrigin::Registry,
                    ..dependency
                })
                .collect(),
            extra: Default::default(),
        })
    })
    .await
}
