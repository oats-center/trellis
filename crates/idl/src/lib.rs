//! Native parser, resolver, canonicalizer, and projector for Trellis source packages.

mod ast;
mod canonical;
mod compatibility;
mod compile;
mod lexer;
mod parser;
pub mod project;
mod projection;
mod semantic;

pub use canonical::{
    api_digest, canonical_package, capability_consent_digest, package_digest, participant_digest,
    selected_surface_digest, CanonicalMode,
};
pub use compatibility::{
    compare_implementation, compare_resource, compare_resource_evolution, compare_selected,
    CompatibilityIssue, CompatibilityReport, ResourceCompatibilityReport, RetainedResourceActual,
    SelectedCompatibilityCache,
};
#[doc(hidden)]
pub use compile::selected_permission_atoms;
pub use compile::SuppliedDependencies;
pub use projection::json_schema;
pub use semantic::*;

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use semver::Version;

    use crate::{
        canonical_package, compile_project,
        project::{GenerateConfig, PackageManifest, PackageMetadata},
        ActionDefinition, CanonicalMode, PackageGraph, Pagination, ResourceDefinition, SourceUnit,
    };

    fn compile(source: impl Into<String>) -> miette::Result<PackageGraph> {
        compile_project(
            &PackageManifest {
                package: PackageMetadata {
                    name: "fixture".into(),
                    version: Version::new(1, 0, 0),
                },
                sources: BTreeMap::from([("main".into(), "main.trellis".into())]),
                dependencies: BTreeMap::new(),
                generate: GenerateConfig::default(),
                default_registry: None,
                registries: BTreeMap::new(),
            },
            vec![SourceUnit {
                alias: "main".into(),
                path: PathBuf::from("main.trellis"),
                source: source.into(),
            }],
            BTreeMap::new(),
        )
    }

    #[test]
    fn compiles_native_cursor_pagination_fixture() {
        let graph = compile(include_str!("../fixtures/cursor-pagination.trellis")).unwrap();

        let action = graph
            .root_package()
            .apis()
            .values()
            .next()
            .unwrap()
            .actions()
            .values()
            .next()
            .unwrap();
        assert!(matches!(
            action,
            ActionDefinition::Rpc {
                pagination: Some(Pagination::Cursor),
                ..
            }
        ));
    }

    #[test]
    fn rejects_removed_job_authoring_knobs() {
        for member in ["features [progress];", "ack_wait 1m;", "heartbeat 1s;"] {
            let source = format!(
                "model Payload {{ value: string; }} service Worker {{ job Work {{ title \"Work\"; description \"Work queue.\"; payload Payload; {member} }} }}"
            );
            assert!(
                compile(source).is_err(),
                "accepted removed Job member: {member}"
            );
        }
    }

    #[test]
    fn job_policy_fixture_round_trips_canonically() {
        let graph = compile(include_str!("../fixtures/job-policy.trellis")).unwrap();
        let resource = graph
            .root_package()
            .participants()
            .values()
            .next()
            .unwrap()
            .resources()
            .values()
            .next()
            .unwrap();
        let ResourceDefinition::Job {
            deadline_ms, retry, ..
        } = resource
        else {
            panic!("expected Job resource")
        };
        assert_eq!(*deadline_ms, Some(45_000));
        assert_eq!(retry.as_ref().unwrap().attempts, 3);
        assert_eq!(retry.as_ref().unwrap().backoff_ms, [5_000, 30_000]);

        let canonical =
            canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap();
        let round_trip = compile(&canonical).unwrap();
        assert_eq!(round_trip.root_digest(), graph.root_digest());
    }

    #[test]
    fn builtin_state_wire_contract_matches_wo04() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../runtime");
        let manifest = crate::project::read_manifest(&root.join("trellis.toml")).unwrap();
        let sources = crate::project::load_sources(&root, &manifest).unwrap();
        let graph = compile_project(&manifest, sources, BTreeMap::new()).unwrap();
        let source =
            crate::canonical_package(&graph, graph.root(), crate::CanonicalMode::Presentation)
                .unwrap();

        assert!(source.contains("type StateRepresentationVersion = uint32(min=1);"));
        assert!(source.contains("createdAt: timestamp;"));
        assert!(source.contains("updatedAt: timestamp;"));
        assert!(source.contains("mode: StatePutMode;"));
        assert!(source.contains("revision?: StateRevision;"));
        assert!(source.contains("value: bytes;"));
        assert!(source.contains("rpc Put {"));
        assert!(!source.contains("rpc Set {"));
        assert!(!source.contains("StateSetRequest"));
    }
}

use project::{Dependency, GenerateConfig, PackageManifest, PackageMetadata};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

/// Compile a source package and supplied exact dependency graphs without I/O.
///
/// # Errors
///
/// Returns source-aware syntax or semantic diagnostics. No partial graph is returned.
pub fn compile_project(
    manifest: &PackageManifest,
    sources: Vec<SourceUnit>,
    dependencies: SuppliedDependencies,
) -> miette::Result<PackageGraph> {
    compile::compile(manifest, sources, dependencies)
}

/// One immutable canonical source package in a bootstrap evidence closure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageSourceEvidence {
    /// Logical package identity.
    pub name: String,
    /// Exact package version.
    pub version: Version,
    /// Semantic package digest.
    pub digest: String,
    /// Presentation-preserving canonical IDL.
    pub source: String,
}

/// Complete exact source-package closure embedded by generated code.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageEvidence {
    /// Root package identity.
    pub root_package: String,
    /// Root semantic package digest.
    pub root_digest: String,
    /// Complete closure sorted by package identity.
    pub packages: Vec<PackageSourceEvidence>,
}

/// Recompile and verify generated package evidence without filesystem or network access.
///
/// # Errors
///
/// Returns an error when the closure is incomplete, cyclic, malformed, or digest-invalid.
pub fn compile_evidence(evidence: PackageEvidence) -> miette::Result<PackageGraph> {
    if !evidence
        .packages
        .windows(2)
        .all(|pair| pair[0].name < pair[1].name)
    {
        return Err(miette::miette!(
            "evidence packages must be uniquely sorted by identity"
        ));
    }
    let entries = evidence
        .packages
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut compiled = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    compile_evidence_package(
        &evidence.root_package,
        &entries,
        &mut compiled,
        &mut visiting,
    )?;
    if compiled.len() != entries.len() {
        return Err(miette::miette!(
            "evidence contains packages outside the root closure"
        ));
    }
    let graph = compiled
        .remove(&evidence.root_package)
        .ok_or_else(|| miette::miette!("evidence root package is absent"))?;
    if graph.root_digest() != evidence.root_digest {
        return Err(miette::miette!(
            "evidence root digest does not match recompiled semantics"
        ));
    }
    Ok(graph)
}

fn compile_evidence_package(
    name: &str,
    entries: &BTreeMap<&str, &PackageSourceEvidence>,
    compiled: &mut BTreeMap<String, PackageGraph>,
    visiting: &mut BTreeSet<String>,
) -> miette::Result<PackageGraph> {
    if let Some(graph) = compiled.get(name) {
        return Ok(graph.clone());
    }
    if !visiting.insert(name.to_owned()) {
        return Err(miette::miette!("evidence package cycle through '{name}'"));
    }
    let entry = entries
        .get(name)
        .ok_or_else(|| miette::miette!("evidence package '{name}' is missing"))?;
    let source = SourceUnit {
        alias: "canonical".into(),
        path: PathBuf::from("canonical.trellis"),
        source: entry.source.clone(),
    };
    let parsed = parser::parse(std::slice::from_ref(&source))?;
    let prelude = &parsed[0].prelude;
    if prelude.package.as_deref() != Some(name) {
        return Err(miette::miette!(
            "evidence source package prelude does not match '{name}'"
        ));
    }
    let mut dependencies = BTreeMap::new();
    let mut supplied = BTreeMap::new();
    for dependency in &prelude.dependencies {
        let graph = compile_evidence_package(&dependency.package, entries, compiled, visiting)?;
        if graph.root_package().version().to_string() != dependency.version
            || graph.root_digest() != dependency.digest
        {
            return Err(miette::miette!(
                "evidence dependency '{}' does not match its canonical prelude",
                dependency.package
            ));
        }
        dependencies.insert(
            dependency.alias.clone(),
            Dependency {
                package: dependency.package.clone(),
                version: None,
                path: Some(dependency.package.clone()),
                registry: None,
            },
        );
        supplied.insert(dependency.alias.clone(), graph);
    }
    let manifest = PackageManifest {
        package: PackageMetadata {
            name: name.to_owned(),
            version: entry.version.clone(),
        },
        sources: BTreeMap::from([("canonical".into(), "canonical.trellis".into())]),
        dependencies,
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    };
    let graph = compile_project(&manifest, vec![source], supplied)?;
    if graph.root_digest() != entry.digest {
        return Err(miette::miette!(
            "evidence digest for '{name}' does not match recompiled semantics"
        ));
    }
    let canonical = canonical_package(&graph, graph.root(), CanonicalMode::Presentation)?;
    if canonical != entry.source {
        return Err(miette::miette!(
            "package '{}' evidence source is not presentation-canonical",
            entry.name
        ));
    }
    visiting.remove(name);
    compiled.insert(name.to_owned(), graph.clone());
    Ok(graph)
}
