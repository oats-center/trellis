//! Package evidence Auth builds from a package's own sources.
//!
//! A client sends the root package's manifest and `.trellis` files, plus any
//! packages it supplies itself. Auth takes every other dependency from the
//! packages it already holds verified source for: the built-in `trellis`
//! package it ships and the packages of every installed participant. It never
//! fetches anything. The evidence it returns is ordinary package evidence, so
//! `Deployments.Create` still recompiles and checks all of it.
//!
//! Every package is chosen by full identity, workspace and name. A
//! dependency that names no workspace matches only `localhost` packages or
//! the built-in `trellis`, unless it is a path dependency on a package the
//! client supplied under that name.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use semver::{Version, VersionReq};
use trellis_idl::project::{
    package_identity, Dependency, GenerateConfig, PackageManifest, PackageMetadata,
};
use trellis_idl::{PackageEvidence, PackageGraph, ParticipantKind, SourceUnit};
use trellis_runtime_apis::__types::trellis::{
    AuthDeploymentKind, AuthMissingPackage, AuthName, AuthPackageOrigin, AuthPackageResolveRequest,
    AuthPackageResolveResponse, AuthPackageSources, AuthRejectedPackage, AuthResolvedPackage,
    AuthResolvedParticipant, AuthShortName,
};

use super::sqlite::{decode, AuthError, Connection};

/// Every package closure Auth holds verified source for: the built-in
/// participants' first, then each installed participant's.
pub(super) fn held_evidence(connection: &Connection) -> Result<Vec<HeldEvidence>, AuthError> {
    let mut held = builtin_evidence()?;
    let mut statement =
        connection.prepare("SELECT evidence_json FROM auth_package_evidence_documents")?;
    for row in statement.query_map([], |row| row.get::<_, String>(0))? {
        held.push(HeldEvidence {
            evidence: decode(&row?)?,
            builtin: false,
        });
    }
    Ok(held)
}

/// The closures of the built-in participants, whose `trellis` package this
/// server ships.
pub(super) fn builtin_evidence() -> Result<Vec<HeldEvidence>, AuthError> {
    Ok(super::initialization::builtin_package_evidence()?
        .into_iter()
        .map(|(_, evidence)| HeldEvidence {
            evidence,
            builtin: true,
        })
        .collect())
}

/// One verified package closure Auth holds.
pub(super) struct HeldEvidence {
    evidence: PackageEvidence,
    builtin: bool,
}

/// A package the root's closure can use, held by Auth or supplied by the client.
struct Candidate {
    /// Full package identity.
    name: String,
    version: Version,
    digest: Option<String>,
    origin: AuthPackageOrigin,
    source: CandidateSource,
}

enum CandidateSource {
    /// Package `entry` of held closure `closure`.
    Held { closure: usize, entry: usize },
    /// Supplied package `index`.
    Supplied(usize),
}

/// One package compiled from a client's files.
struct Authored {
    manifest: PackageManifest,
    sources: Vec<SourceUnit>,
    /// Its position in the request's `packages`; `None` for the root.
    supplied: Option<usize>,
}

/// What a package asks of one of its dependencies.
struct Requirement {
    by: String,
    version: Option<VersionReq>,
    digest: Option<String>,
}

impl Requirement {
    fn admits(&self, candidate: &Candidate) -> bool {
        self.version
            .as_ref()
            .is_none_or(|requirement| requirement.matches(&candidate.version))
            && match (&self.digest, &candidate.digest) {
                (Some(wanted), Some(digest)) => wanted == digest,
                _ => true,
            }
    }
}

/// Build the root package's complete evidence, or say which dependencies no
/// package Auth holds or the client supplied satisfies.
pub(super) fn resolve(
    held: Vec<HeldEvidence>,
    input: &AuthPackageResolveRequest,
) -> Result<AuthPackageResolveResponse, AuthError> {
    let supplied = input.packages.as_deref().unwrap_or_default();
    let mut candidates = Vec::<Candidate>::new();
    let mut digests = BTreeSet::new();
    for (closure, held) in held.iter().enumerate() {
        for (entry, package) in held.evidence.packages.iter().enumerate() {
            if digests.insert(package.digest.clone()) {
                candidates.push(Candidate {
                    name: package.name.clone(),
                    version: package.version.clone(),
                    digest: Some(package.digest.clone()),
                    origin: if held.builtin {
                        AuthPackageOrigin::Builtin
                    } else {
                        AuthPackageOrigin::Installed
                    },
                    source: CandidateSource::Held { closure, entry },
                });
            }
        }
    }

    // Supplied packages' identities come first: a root without a manifest
    // may import one of them.
    let mut supplied_names = Vec::with_capacity(supplied.len());
    for package in supplied {
        let (name, _) = identity(package)?;
        if supplied_names.contains(&name) {
            return Err(AuthError::Invalid(format!(
                "package {name} was supplied more than once"
            )));
        }
        supplied_names.push(name);
    }
    let known = candidates
        .iter()
        .map(|candidate| candidate.name.clone())
        .chain(supplied_names.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut unknown_imports = BTreeSet::new();
    let root = authored(&input.root, &known, &supplied_names, &mut unknown_imports)?;
    let root_identity = root.manifest.identity();
    if supplied_names.contains(&root_identity) {
        return Err(AuthError::Invalid(format!(
            "package {root_identity} is both the root and a supplied package"
        )));
    }
    let mut authored_packages = Vec::with_capacity(supplied.len());
    for package in supplied {
        let mut package = authored(package, &known, &supplied_names, &mut unknown_imports)?;
        package.supplied = Some(authored_packages.len());
        candidates.push(Candidate {
            name: package.manifest.identity(),
            version: package.manifest.package.version.clone(),
            digest: None,
            origin: AuthPackageOrigin::Supplied,
            source: CandidateSource::Supplied(authored_packages.len()),
        });
        authored_packages.push(package);
    }

    let mut graphs = BTreeMap::<usize, Arc<PackageGraph>>::new();
    let mut held_graph = |closure: usize| -> Result<Arc<PackageGraph>, AuthError> {
        if let Some(graph) = graphs.get(&closure) {
            return Ok(Arc::clone(graph));
        }
        let graph = trellis_idl::compile_evidence(held[closure].evidence.clone())
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        graphs.insert(closure, Arc::clone(&graph));
        Ok(graph)
    };

    // Choose one package per name until the choices stop changing: a chosen
    // held package pins its own dependencies exactly, which can change the
    // choice for a name another package asked for more loosely.
    let mut chosen = BTreeMap::<String, usize>::new();
    let mut requirements = BTreeMap::<String, Vec<Requirement>>::new();
    let mut settled = false;
    for _ in 0..64 {
        requirements.clear();
        let mut queue = VecDeque::from([None::<usize>]);
        let mut visited = BTreeSet::new();
        while let Some(next) = queue.pop_front() {
            let asks = match next {
                None => manifest_requirements(&root.manifest, &supplied_names)?,
                Some(index) => match candidates[index].source {
                    CandidateSource::Supplied(package) => manifest_requirements(
                        &authored_packages[package].manifest,
                        &supplied_names,
                    )?,
                    CandidateSource::Held { closure, .. } => {
                        let graph = held_graph(closure)?;
                        let package = graph
                            .packages()
                            .values()
                            .find(|package| package.identity().as_str() == candidates[index].name)
                            .ok_or(AuthError::Unavailable)?;
                        package
                            .dependencies()
                            .iter()
                            .map(|(id, dependency)| {
                                Ok((
                                    id.as_str().to_owned(),
                                    Requirement {
                                        by: candidates[index].name.clone(),
                                        version: Some(
                                            VersionReq::parse(&format!("={}", dependency.version))
                                                .map_err(|error| {
                                                    AuthError::Invalid(error.to_string())
                                                })?,
                                        ),
                                        digest: Some(dependency.digest.clone()),
                                    },
                                ))
                            })
                            .collect::<Result<Vec<_>, AuthError>>()?
                    }
                },
            };
            for (name, requirement) in asks {
                if name == root_identity {
                    return Err(AuthError::Invalid(format!(
                        "package {name} depends on itself through {}",
                        requirement.by
                    )));
                }
                if visited.insert(name.clone()) {
                    if let Some(&index) = chosen.get(&name) {
                        queue.push_back(Some(index));
                    }
                }
                requirements.entry(name).or_default().push(requirement);
            }
        }
        let next = requirements
            .iter()
            .filter_map(|(name, asks)| {
                let admitted = candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| {
                        &candidate.name == name && asks.iter().all(|ask| ask.admits(candidate))
                    })
                    // A package the client supplied wins over every package
                    // Auth holds; among those, the highest version, then the
                    // built-in copy.
                    .max_by(|(_, left), (_, right)| {
                        (left.origin == AuthPackageOrigin::Supplied)
                            .cmp(&(right.origin == AuthPackageOrigin::Supplied))
                            .then_with(|| left.version.cmp(&right.version))
                            .then_with(|| {
                                (left.origin == AuthPackageOrigin::Builtin)
                                    .cmp(&(right.origin == AuthPackageOrigin::Builtin))
                            })
                    })
                    .map(|(index, _)| index)?;
                Some((name.clone(), admitted))
            })
            .collect::<BTreeMap<_, _>>();
        if next == chosen {
            settled = true;
            break;
        }
        chosen = next;
    }
    if !settled {
        return Err(AuthError::Invalid(
            "the dependencies ask for versions of the same package that cannot all be met".into(),
        ));
    }
    // The last pass settled, so its requirements describe `chosen`.
    let mut missing = Vec::new();
    let mut dependencies = Vec::new();
    for (name, asks) in &requirements {
        let Some(&index) = chosen.get(name) else {
            let mut available = candidates
                .iter()
                .filter(|candidate| &candidate.name == name)
                .map(|candidate| candidate.version.clone())
                .collect::<Vec<_>>();
            available.sort_by(|left, right| right.cmp(left));
            available.dedup();
            missing.push(AuthMissingPackage {
                name: AuthName(name.clone()),
                requirements: asks
                    .iter()
                    .filter_map(|ask| ask.version.as_ref().map(ToString::to_string))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                required_by: asks
                    .iter()
                    .map(|ask| ask.by.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(AuthName)
                    .collect(),
                available_versions: available
                    .iter()
                    .map(|version| AuthShortName(version.to_string()))
                    .collect(),
                extra: Default::default(),
            });
            continue;
        };
        let choice = &candidates[index];
        let mut others = candidates
            .iter()
            .filter(|candidate| {
                &candidate.name == name
                    && candidate.version != choice.version
                    && asks.iter().all(|ask| ask.admits(candidate))
            })
            .map(|candidate| candidate.version.clone())
            .collect::<Vec<_>>();
        others.sort_by(|left, right| right.cmp(left));
        others.dedup();
        dependencies.push(AuthResolvedPackage {
            name: AuthName(name.clone()),
            version: AuthShortName(choice.version.to_string()),
            origin: choice.origin.clone(),
            other_versions: others
                .iter()
                .map(|version| AuthShortName(version.to_string()))
                .collect(),
            extra: Default::default(),
        });
    }
    if !missing.is_empty() || !unknown_imports.is_empty() {
        return Ok(AuthPackageResolveResponse {
            package_evidence: None,
            participants: Vec::new(),
            dependencies,
            missing,
            unknown_imports: unknown_imports.into_iter().collect(),
            rejected: Vec::new(),
            extra: Default::default(),
        });
    }

    // Compile every chosen package from the leaves up, then the root. A
    // supplied package that doesn't compile is reported by name, so a client
    // can tell which of the packages it sent is at fault.
    let mut compiled = BTreeMap::<String, PackageGraph>::new();
    let mut compiling = BTreeSet::<String>::new();
    let mut rejected = None::<AuthRejectedPackage>;
    #[allow(clippy::too_many_arguments)]
    fn compile(
        package: &Authored,
        supplied_names: &[String],
        chosen: &BTreeMap<String, usize>,
        candidates: &[Candidate],
        authored_packages: &[Authored],
        held: &[HeldEvidence],
        held_graph: &mut dyn FnMut(usize) -> Result<Arc<PackageGraph>, AuthError>,
        compiled: &mut BTreeMap<String, PackageGraph>,
        compiling: &mut BTreeSet<String>,
        rejected: &mut Option<AuthRejectedPackage>,
    ) -> Result<PackageGraph, AuthError> {
        let name = &package.manifest.identity();
        if let Some(graph) = compiled.get(name) {
            return Ok(graph.clone());
        }
        if !compiling.insert(name.clone()) {
            return Err(AuthError::Invalid(format!(
                "the supplied packages depend on each other in a cycle through {name}"
            )));
        }
        let mut supplied = BTreeMap::new();
        for (alias, dependency) in &package.manifest.dependencies {
            let index = chosen[&dependency_key(dependency, supplied_names)];
            let graph = match candidates[index].source {
                CandidateSource::Supplied(other) => compile(
                    &authored_packages[other],
                    supplied_names,
                    chosen,
                    candidates,
                    authored_packages,
                    held,
                    held_graph,
                    compiled,
                    compiling,
                    rejected,
                )?,
                CandidateSource::Held { closure, entry } => {
                    held_package(&held[closure].evidence, entry, &*held_graph(closure)?)?
                }
            };
            supplied.insert(alias.clone(), graph);
        }
        let graph =
            trellis_idl::compile_project(&package.manifest, package.sources.clone(), supplied)
                .map_err(|error| {
                    if let (Some(index), None) = (package.supplied, &rejected) {
                        *rejected = Some(AuthRejectedPackage {
                            index: index as u32,
                            name: AuthName(name.clone()),
                            reason: error.to_string(),
                            extra: Default::default(),
                        });
                    }
                    AuthError::Invalid(format!("{name}: {error}"))
                })?;
        compiling.remove(name);
        compiled.insert(name.clone(), graph.clone());
        Ok(graph)
    }
    let graph = match compile(
        &root,
        &supplied_names,
        &chosen,
        &candidates,
        &authored_packages,
        &held,
        &mut held_graph,
        &mut compiled,
        &mut compiling,
        &mut rejected,
    ) {
        Ok(graph) => graph,
        Err(_) if rejected.is_some() => {
            return Ok(AuthPackageResolveResponse {
                package_evidence: None,
                participants: Vec::new(),
                dependencies,
                missing,
                unknown_imports: Vec::new(),
                rejected: rejected.into_iter().collect(),
                extra: Default::default(),
            })
        }
        Err(error) => return Err(error),
    };
    let evidence = trellis_idl::package_evidence(&graph)
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    // The same check Deployments.Create makes, so evidence returned here is
    // evidence it accepts.
    trellis_idl::compile_evidence(evidence.clone()).map_err(|error| {
        AuthError::Invalid(format!(
            "the dependencies chosen for {} do not fit together: {error}",
            graph.root()
        ))
    })?;
    let participants = graph
        .root_package()
        .participants()
        .values()
        .map(|participant| {
            Ok(AuthResolvedParticipant {
                participant_id: participant.identity().as_str().to_owned(),
                kind: match participant.kind() {
                    ParticipantKind::Service => AuthDeploymentKind::Service,
                    ParticipantKind::Device => AuthDeploymentKind::Device,
                },
                // So settings can be entered with the right types before the
                // deployment exists.
                config: participant
                    .config()
                    .iter()
                    .map(|(name, field)| {
                        Ok(serde_json::from_value(serde_json::Value::Object(
                            super::config::declaration(&graph, name, field)?,
                        ))?)
                    })
                    .collect::<Result<_, AuthError>>()?,
                extra: Default::default(),
            })
        })
        .collect::<Result<_, AuthError>>()?;
    Ok(AuthPackageResolveResponse {
        package_evidence: Some(serde_json::from_value(serde_json::to_value(evidence)?)?),
        participants,
        dependencies,
        missing,
        unknown_imports: Vec::new(),
        rejected: Vec::new(),
        extra: Default::default(),
    })
}

/// The graph of one package of a held closure, compiled from that package's
/// own sub-closure.
fn held_package(
    evidence: &PackageEvidence,
    entry: usize,
    closure: &PackageGraph,
) -> Result<PackageGraph, AuthError> {
    let root = &evidence.packages[entry];
    let mut needed = BTreeSet::from([root.name.clone()]);
    let mut queue = VecDeque::from([root.name.clone()]);
    while let Some(name) = queue.pop_front() {
        let package = closure
            .packages()
            .values()
            .find(|package| package.identity().as_str() == name)
            .ok_or(AuthError::Unavailable)?;
        for id in package.dependencies().keys() {
            if needed.insert(id.as_str().to_owned()) {
                queue.push_back(id.as_str().to_owned());
            }
        }
    }
    let graph = trellis_idl::compile_evidence(PackageEvidence {
        root_package: root.name.clone(),
        root_digest: root.digest.clone(),
        packages: evidence
            .packages
            .iter()
            .filter(|package| needed.contains(&package.name))
            .cloned()
            .collect(),
    })
    .map_err(|error| AuthError::Invalid(error.to_string()))?;
    Ok((*graph).clone())
}

/// The full identity a dependency asks for. A registry dependency names it;
/// a path dependency names only a package, which is the supplied package of
/// that name when there is exactly one, and otherwise the `localhost` (or
/// built-in) package of that name.
fn dependency_key(dependency: &Dependency, supplied: &[String]) -> String {
    if let Some(identity) = dependency.identity() {
        return identity;
    }
    let mut named = supplied
        .iter()
        .filter(|identity| trellis_protocol::short_id(identity) == dependency.package);
    match (named.next(), named.next()) {
        (Some(only), None) => only.clone(),
        _ => package_identity(None, &dependency.package),
    }
}

/// The dependencies a manifest names, keyed by full identity.
fn manifest_requirements(
    manifest: &PackageManifest,
    supplied: &[String],
) -> Result<Vec<(String, Requirement)>, AuthError> {
    manifest
        .dependencies
        .values()
        .map(|dependency| {
            Ok((
                dependency_key(dependency, supplied),
                Requirement {
                    by: manifest.identity(),
                    version: dependency
                        .version
                        .as_deref()
                        .map(VersionReq::parse)
                        .transpose()
                        .map_err(|error| AuthError::Invalid(error.to_string()))?,
                    digest: None,
                },
            ))
        })
        .collect()
}

/// A supplied package's full identity and version, from its manifest or its
/// fields. Without a manifest, `name` is a full identity or a bare name, which
/// is a `localhost` package.
fn identity(package: &AuthPackageSources) -> Result<(String, Version), AuthError> {
    match &package.manifest {
        Some(text) => {
            let manifest = parse_manifest(text)?;
            Ok((manifest.identity(), manifest.package.version))
        }
        None => match (&package.name, &package.version) {
            (Some(name), Some(version)) => Ok((
                if name.0.contains('/') {
                    trellis_idl::project::split_package_identity(&name.0)
                        .map_err(|error| AuthError::Invalid(error.to_string()))?;
                    name.0.clone()
                } else {
                    package_identity(None, &name.0)
                },
                Version::parse(version).map_err(|error| {
                    AuthError::Invalid(format!("version {}: {error}", version.0))
                })?,
            )),
            _ => Err(AuthError::Invalid(
                "a package without trellis.toml needs a name and a version".into(),
            )),
        },
    }
}

fn parse_manifest(text: &str) -> Result<PackageManifest, AuthError> {
    let manifest = toml::from_str::<PackageManifest>(text)
        .map_err(|error| AuthError::Invalid(format!("trellis.toml: {error}")))?;
    manifest
        .validate()
        .map_err(|error| AuthError::Invalid(format!("trellis.toml: {error}")))?;
    Ok(manifest)
}

/// Read one package's files. With a manifest, its listed sources are taken
/// from the files by path, or by file name when only one file has it. Without
/// one, every `.trellis` file is a source named after its file, and each
/// import from outside the package names the package it comes from; an
/// import naming no package Auth knows of is reported, since only the
/// package's own trellis.toml can say what it is.
fn authored(
    package: &AuthPackageSources,
    known: &BTreeSet<String>,
    supplied: &[String],
    unknown: &mut BTreeSet<String>,
) -> Result<Authored, AuthError> {
    let normalized = |path: &str| path.trim_start_matches("./").replace('\\', "/");
    let files = package
        .files
        .iter()
        .map(|file| (normalized(&file.path), file.text.as_str()))
        .collect::<Vec<_>>();
    let file_name = |path: &str| {
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    if let Some(text) = &package.manifest {
        let manifest = parse_manifest(text)?;
        let mut sources = Vec::with_capacity(manifest.sources.len());
        for (alias, path) in &manifest.sources {
            let wanted = normalized(path);
            let found = files
                .iter()
                .find(|(path, _)| *path == wanted)
                .or_else(|| {
                    let mut named = files
                        .iter()
                        .filter(|(path, _)| file_name(path) == file_name(&wanted));
                    match (named.next(), named.next()) {
                        (Some(file), None) => Some(file),
                        _ => None,
                    }
                })
                .ok_or_else(|| {
                    AuthError::Invalid(format!(
                        "{}'s trellis.toml lists {path}, which is not among its files",
                        manifest.identity()
                    ))
                })?;
            sources.push(SourceUnit {
                alias: alias.clone(),
                path: PathBuf::from(path),
                source: found.1.to_owned(),
            });
        }
        return Ok(Authored {
            manifest,
            sources,
            supplied: None,
        });
    }
    let (name, version) = identity(package)?;
    let (workspace, package_name) = trellis_idl::project::split_package_identity(&name)
        .map(|(workspace, name)| {
            (
                workspace
                    .filter(|workspace| *workspace != trellis_protocol::LOCAL_WORKSPACE)
                    .map(str::to_owned),
                name.to_owned(),
            )
        })
        .map_err(|error| AuthError::Invalid(error.to_string()))?;
    let mut sources = BTreeMap::new();
    for (path, text) in &files {
        if Path::new(path)
            .extension()
            .is_none_or(|ext| ext != "trellis")
        {
            continue;
        }
        let alias = Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        if sources
            .insert(
                alias.clone(),
                SourceUnit {
                    alias: alias.clone(),
                    path: PathBuf::from(path),
                    source: (*text).to_owned(),
                },
            )
            .is_some()
        {
            return Err(AuthError::Invalid(format!(
                "two files of {name} are named {alias}.trellis; add its trellis.toml"
            )));
        }
    }
    let sources = sources.into_values().collect::<Vec<_>>();
    let mut dependencies = BTreeMap::new();
    for import in trellis_idl::external_imports(&sources)
        .map_err(|error| AuthError::Invalid(format!("{name}: {error}")))?
    {
        // An import names a package by its name alone, so it means the
        // supplied package of that name, or a `localhost` or built-in one.
        let dependency = Dependency {
            package: import.clone(),
            workspace: None,
            version: None,
            path: Some(import.clone()),
        };
        let key = dependency_key(&dependency, supplied);
        if known.contains(&key) && key != name {
            dependencies.insert(import, dependency);
        } else {
            unknown.insert(import);
        }
    }
    let manifest = PackageManifest {
        package: PackageMetadata {
            name: package_name,
            workspace,
            version,
        },
        sources: sources
            .iter()
            .map(|source| {
                (
                    source.alias.clone(),
                    source.path.to_string_lossy().into_owned(),
                )
            })
            .collect(),
        dependencies,
        generate: GenerateConfig::default(),
    };
    manifest.validate().map_err(|error| {
        AuthError::Invalid(format!(
            "{}: {error}; add its trellis.toml",
            manifest.identity()
        ))
    })?;
    Ok(Authored {
        manifest,
        sources,
        supplied: None,
    })
}
