//! Native parser, resolver, canonicalizer, and projector for Trellis source packages.

mod ast;
mod canonical;
mod compatibility;
mod compile;
pub mod distribution;
mod documentation;
mod lexer;
mod parser;
pub mod project;
mod projection;
mod semantic;
pub mod webhook;

pub use canonical::{
    api_digest, canonical_package, package_digest, participant_digest, selected_surface_digest,
    CanonicalMode,
};
pub use compatibility::{
    compare_implementation, compare_resource, compare_resource_evolution, compare_selected,
    CompatibilityIssue, CompatibilityReport, ResourceCompatibilityReport, RetainedResourceActual,
    SelectedCompatibilityCache,
};
pub use compile::SuppliedDependencies;
pub use documentation::{
    action_documentation, api_documentation, field_documentation, type_documentation,
};
pub use projection::{
    described_expression_json_schema, described_json_schema, expression_json_schema, json_schema,
};
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
                    workspace: None,
                    version: Version::new(1, 0, 0),
                },
                sources: BTreeMap::from([("main".into(), "main.trellis".into())]),
                dependencies: BTreeMap::new(),
                generate: GenerateConfig::default(),
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

    fn config_contract(log_level: &str) -> String {
        format!(
            r#"
            enum LogLevel {{ "info"; "debug"; }}
            service Orders {{
              config databaseUrl {{ title "Database URL"; description "Where orders live."; type string; restart; }}
              config databasePassword {{ title "Database password"; description "Role password."; type string; secret; restart; }}
              config webhookToken {{ title "Webhook token"; description "Signs outgoing hooks."; type string | null; secret; }}
              config logLevel {{ {log_level} }}
              config batchSize {{ title "Batch size"; description "Rows per write."; type int32; default 50; }}
            }}
            "#
        )
    }

    #[test]
    fn config_fields_resolve_round_trip_and_change_the_build_only_by_shape() {
        let level = r#"title "Log level"; description "Lowest level written."; type LogLevel; default "info";"#;
        let graph = compile(config_contract(level)).unwrap();
        let participant = graph.root_package().participants().values().next().unwrap();
        let config = participant.config();
        assert_eq!(
            config.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "batchSize",
                "databasePassword",
                "databaseUrl",
                "logLevel",
                "webhookToken"
            ]
        );
        assert!(config["databasePassword"].secret && config["databasePassword"].restart);
        assert_eq!(config["batchSize"].default, Some(serde_json::json!(50)));
        assert_eq!(config["databaseUrl"].default, None);
        // A nullable field is optional, a secret too: unset, it reads null.
        assert_eq!(
            config["webhookToken"].default,
            Some(serde_json::Value::Null)
        );

        let canonical =
            canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap();
        let round_trip = compile(&canonical).unwrap();
        assert_eq!(round_trip.root_digest(), graph.root_digest());
        let id = participant.identity();
        let digest = crate::participant_digest(&graph, id).unwrap();
        assert_eq!(crate::participant_digest(&round_trip, id).unwrap(), digest);

        // Retitling a field is presentation; changing its default is a new build.
        let retitled = compile(config_contract(&level.replace("Log level", "Verbosity"))).unwrap();
        assert_eq!(crate::participant_digest(&retitled, id).unwrap(), digest);
        let redefaulted =
            compile(config_contract(&level.replace(r#""info";"#, r#""debug";"#))).unwrap();
        assert_ne!(crate::participant_digest(&redefaulted, id).unwrap(), digest);
    }

    #[test]
    fn config_refuses_secrets_outside_services_and_defaults_apps_cannot_wait_for() {
        let device = r#"device Sensor { config token { title "Token"; description "Upload token."; type string; secret; } }"#;
        let error = compile(device).unwrap_err().to_string();
        assert!(error.contains("only services can keep a secret"), "{error}");
        let wrong = r#"service Worker { config size { title "Size"; description "Batch size."; type int32; default "large"; } }"#;
        let error = compile(wrong).unwrap_err().to_string();
        assert!(error.contains("does not match its type"), "{error}");
        let app = r#"app Panel { config theme { title "Theme"; description "Panel theme."; type string; } }"#;
        let error = compile(app).unwrap_err().to_string();
        assert!(error.contains("needs a default"), "{error}");
        let published = r#"service Worker { config key { title "Key"; description "API key."; type string; secret; default "abc"; } }"#;
        let error = compile(published).unwrap_err().to_string();
        assert!(error.contains("cannot have a default"), "{error}");
    }

    #[test]
    fn config_defaults_and_optionality_follow_resolved_named_types() {
        let field = |ty: &str, rest: &str| {
            format!(
                r#"enum LogLevel {{ "info"; "debug"; }}
                type Port = int32(min=1, max=65535);
                type OptionalKey = string | null;
                type Key = OptionalKey;
                service Worker {{ config value {{
                  title "Value"; description "A value."; type {ty}; {rest}
                }} }}"#
            )
        };
        for (ty, default) in [("LogLevel", "42"), ("Port", "0"), ("Key", "7")] {
            let error = compile(field(ty, &format!("default {default};")))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("does not match its type"),
                "{ty} {default}: {error}"
            );
        }
        let default = |source: String| {
            let graph = compile(source).unwrap();
            let participant = graph.root_package().participants().values().next().unwrap();
            participant.config()["value"].default.clone()
        };
        assert_eq!(
            default(field("LogLevel", r#"default "debug";"#)),
            Some(serde_json::json!("debug"))
        );
        assert_eq!(
            default(field("Port", "default 8080;")),
            Some(serde_json::json!(8080))
        );
        // A type that is nullable through aliases is optional, so a secret of
        // it is too; nothing waits for either.
        for rest in ["", "secret;"] {
            assert_eq!(
                default(field("Key", rest)),
                Some(serde_json::Value::Null),
                "{rest}"
            );
        }
        assert_eq!(default(field("Port", "")), None);
    }

    fn transfer_contract(member: &str) -> String {
        format!(
            r#"
            model Value {{ value: string; }}
            api files@v1 {{
              title "Files";
              description "Transfer fixture.";
              operation Ingest {{ input Value; output Value; {member} }}
              capabilities {{
                capability files.write {{ title "Write"; description "Send bytes."; grants "Stores bytes."; allows {{ operation Ingest; }} }}
              }}
            }}
            "#
        )
    }

    #[test]
    fn stream_operations_accept_caller_bytes_with_their_own_digest() {
        let graph = compile(transfer_contract("stream;")).unwrap();
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
            ActionDefinition::Operation {
                upload: true,
                stream: true,
                ..
            }
        ));
        let canonical =
            canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap();
        assert!(canonical.contains("stream;") && !canonical.contains("upload;"));
        assert_eq!(
            compile(&canonical).unwrap().root_digest(),
            graph.root_digest()
        );
        let staged = compile(transfer_contract("upload;")).unwrap();
        assert_ne!(staged.root_digest(), graph.root_digest());
    }

    fn grouped_contract(capabilities: &str) -> String {
        format!(
            r#"
            model Value {{ value: string; }}
            api files@v1 {{
              title "Files";
              description "File fixture.";
              rpc Get {{ input Value; output Value; }}
              rpc Put {{ input Value; output Value; }}
              capabilities {{ {capabilities} }}
            }}
            "#
        )
    }

    const READ: &str = r#"capability files.read { title "See files"; description "List files."; grants "Reveals files."; allows { rpc Get; } }"#;
    const WRITE: &str = r#"capability files.write { title "Change files"; description "Write files."; grants "Changes files."; allows { rpc Put; } }"#;

    #[test]
    fn groups_bundle_capabilities_and_are_part_of_the_api_meaning() {
        let group = r#"group editor { title "Editor"; description "Read and change files."; grants "Can change files."; capability files.read; capability files.write; }"#;
        let grouped = compile(grouped_contract(&format!("{READ} {WRITE} {group}"))).unwrap();
        let api = grouped.root_package().apis().values().next().unwrap();
        let id = crate::CapabilityId::new("localhost/fixture.files.v1:editor");
        let members: Vec<_> = api.groups()[&id]
            .members
            .iter()
            .map(crate::CapabilityId::as_str)
            .collect();
        assert_eq!(
            members,
            [
                "localhost/fixture.files.v1:files.read",
                "localhost/fixture.files.v1:files.write"
            ]
        );
        assert_eq!(api.selection_allows(&id).unwrap().len(), 2);

        // Both canonical forms keep groups, and a group changes the API digest.
        let canonical =
            canonical_package(&grouped, grouped.root(), CanonicalMode::Presentation).unwrap();
        let round_trip = compile(&canonical).unwrap();
        assert_eq!(round_trip.root_digest(), grouped.root_digest());
        let ungrouped = compile(grouped_contract(&format!("{READ} {WRITE}"))).unwrap();
        assert_ne!(grouped.root_digest(), ungrouped.root_digest());
    }

    #[test]
    fn groups_require_consent_text_and_own_capabilities() {
        for (group, message) in [
            (
                r#"group editor { description "D"; grants "C"; capability files.read; }"#,
                "requires title, description, and grants",
            ),
            (
                r#"group editor { title "T"; description "D"; grants "C"; }"#,
                "has no capabilities",
            ),
            (
                r#"group editor { title "T"; description "D"; grants "C"; capability files.missing; }"#,
                "not a capability of this API",
            ),
            (
                r#"public { allows { rpc Put; } } group public { title "T"; description "D"; grants "C"; capability files.read; }"#,
                "reserved for the public capability",
            ),
        ] {
            let error = compile(grouped_contract(&format!("{READ} {group}"))).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }

    #[test]
    fn operations_cannot_declare_both_upload_and_stream() {
        let error = compile(transfer_contract("upload; stream;")).unwrap_err();
        assert!(error.to_string().contains("both 'upload' and 'stream'"));
    }
}

use project::{Dependency, GenerateConfig, PackageManifest, PackageMetadata};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
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
    /// Full package identity, `<workspace>/<name>`, or `trellis` for the
    /// built-in package.
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
    /// Root package's full identity.
    pub root_package: String,
    /// Root semantic package digest.
    pub root_digest: String,
    /// Complete closure sorted by package identity.
    pub packages: Vec<PackageSourceEvidence>,
}

// Only successful compilations of exactly equal complete evidence are reusable.
// Keep compilation outside the lock; concurrent misses may compile twice, but
// never serialize unrelated evidence behind the compiler.
static COMPILED_EVIDENCE: Mutex<VecDeque<(PackageEvidence, Arc<PackageGraph>)>> =
    Mutex::new(VecDeque::new());

/// Compile and verify generated package evidence without filesystem or network access.
///
/// Every package and every dependency edge is found by full identity,
/// workspace and name, so two packages with the same name in different
/// workspaces stay apart.
///
/// A bounded process-local cache reuses successful compilations only when the
/// complete evidence matches, including every source byte and dependency. Claimed
/// digests alone never authorize reuse. No policy or authorization state is cached.
/// The graph is shared with the cache, so a hit costs no copy.
///
/// # Errors
///
/// Returns an error when the closure is incomplete, cyclic, malformed, or digest-invalid.
pub fn compile_evidence(evidence: PackageEvidence) -> miette::Result<Arc<PackageGraph>> {
    let cached = COMPILED_EVIDENCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(input, _)| input == &evidence)
        .map(|(_, graph)| Arc::clone(graph));
    if let Some(graph) = cached {
        return Ok(graph);
    }
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
    let graph = Arc::new(graph);
    let mut cache = COMPILED_EVIDENCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cache.iter().any(|(input, _)| input == &evidence) {
        if cache.len() == 16 {
            cache.pop_front();
        }
        cache.push_back((evidence, Arc::clone(&graph)));
    }
    Ok(graph)
}

/// Build the evidence of a compiled graph: its complete package closure, each
/// package once in identity order with presentation-canonical source.
///
/// # Errors
///
/// Returns an error when a package cannot be rendered canonically.
pub fn package_evidence(graph: &PackageGraph) -> miette::Result<PackageEvidence> {
    Ok(PackageEvidence {
        root_package: graph.root().as_str().to_owned(),
        root_digest: graph.root_digest().to_owned(),
        packages: graph
            .packages()
            .iter()
            .map(|(id, package)| {
                Ok(PackageSourceEvidence {
                    name: id.as_str().to_owned(),
                    version: package.version().clone(),
                    digest: graph
                        .digest(id)
                        .expect("compiled package digest")
                        .to_owned(),
                    source: canonical_package(graph, id, CanonicalMode::Presentation)?,
                })
            })
            .collect::<miette::Result<_>>()?,
    })
}

/// Readable generated names that never collide, for a set of full IDs.
///
/// Each ID starts from its `preferred` name. Only the IDs whose name is shared,
/// ignoring case, are renamed: first to `name` of the short ID (the full ID
/// without its workspace), then to `name` of the full ID. Different full IDs
/// can still produce one name, since `name` drops punctuation
/// (`ghcr.io/acme-lab` and `ghcr.io/acme/lab` both become `GhcrIoAcmeLab`), so
/// names that still collide take `_` and the first eight hex digits of the
/// SHA-256 of their full ID. Every other name stays as it was.
pub fn generated_names<'a>(
    preferred: impl IntoIterator<Item = (&'a str, String)>,
    name: impl Fn(&str) -> String,
) -> BTreeMap<&'a str, String> {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut names = preferred.into_iter().collect::<BTreeMap<_, _>>();
    let shared = |names: &BTreeMap<&'a str, String>| {
        let mut counts = BTreeMap::<String, usize>::new();
        for value in names.values() {
            *counts.entry(value.to_lowercase()).or_default() += 1;
        }
        names
            .iter()
            .filter(|(_, value)| counts[&value.to_lowercase()] > 1)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    let forms: [&dyn Fn(&str) -> String; 2] =
        [&|id| name(trellis_protocol::short_id(id)), &|id| name(id)];
    for form in forms {
        for id in shared(&names) {
            names.insert(id, form(id));
        }
    }
    for id in shared(&names) {
        let digest = Sha256::digest(id.as_bytes());
        let mut suffix = String::new();
        for byte in &digest[..4] {
            write!(suffix, "{byte:02x}").expect("writing to a String cannot fail");
        }
        let value = format!("{}_{suffix}", names[id]);
        names.insert(id, value);
    }
    names
}

/// Name what a package's sources import from outside the package: every
/// `import ... from X` whose `X` is not another source of the same set.
///
/// # Errors
///
/// Returns source-aware syntax diagnostics.
pub fn external_imports(sources: &[SourceUnit]) -> miette::Result<BTreeSet<String>> {
    let parsed = parser::parse(sources)?;
    let own = parsed
        .iter()
        .map(|unit| unit.alias.as_str())
        .collect::<BTreeSet<_>>();
    Ok(parsed
        .iter()
        .flat_map(|unit| unit.imports.values())
        .filter(|import| !own.contains(import.from.as_str()))
        .map(|import| import.from.clone())
        .collect())
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
    let (workspace, package_name) = project::split_package_identity(name)
        .map_err(|error| miette::miette!("evidence package '{name}': {error}"))?;
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
        // The edge is supplied as a path dependency by name; the canonical
        // prelude then pins its full identity, version, and digest.
        dependencies.insert(
            dependency.alias.clone(),
            Dependency {
                package: graph.root().name().to_owned(),
                workspace: None,
                version: None,
                path: Some(dependency.package.clone()),
            },
        );
        supplied.insert(dependency.alias.clone(), graph);
    }
    let manifest = PackageManifest {
        package: PackageMetadata {
            name: package_name.to_owned(),
            workspace: workspace
                .filter(|workspace| *workspace != trellis_protocol::LOCAL_WORKSPACE)
                .map(str::to_owned),
            version: entry.version.clone(),
        },
        sources: BTreeMap::from([("canonical".into(), "canonical.trellis".into())]),
        dependencies,
        generate: GenerateConfig::default(),
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
