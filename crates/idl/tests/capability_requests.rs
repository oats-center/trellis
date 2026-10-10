//! Behavior of whole-capability selection and authored consent revisions.

use std::{collections::BTreeMap, path::PathBuf};

use semver::Version;
use trellis_idl::{
    canonical_package, compare_implementation, compile_project,
    project::{GenerateConfig, PackageManifest, PackageMetadata},
    CanonicalMode, PackageGraph, SourceUnit,
};

const SOURCE: &str = r#"
model Document { id: string; }
api documents@v1 {
  title "Documents";
  description "Document management.";
  rpc Get { input Document; output Document; }
  rpc Put { input Document; output Document; }
  rpc Export { input Document; output Document; }
  capabilities {
    capability read {
      title "Read"; description "Read a document.";
      consequence "The caller sees document contents.";
      consent_revision 2;
      allows { rpc Get; }
    }
    capability write {
      title "Write"; description "Update a document.";
      consequence "The caller can overwrite document contents.";
      consent_revision 1;
      allows { rpc Put; }
    }
    capability export {
      title "Export"; description "Export a document.";
      consequence "The caller can retain document contents elsewhere.";
      consent_revision 1;
      allows { rpc Export; }
    }
  }
}
service Reader {
  use documents { required capability read; capability export; }
}
"#;

fn compile(source: &str) -> miette::Result<PackageGraph> {
    compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: "capability-fixture".into(),
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
fn capability_selection_exposes_only_selected_surfaces_and_round_trips() {
    let source = format!("{SOURCE}\napp ReaderApp {{\nuse documents {{ required capability read; capability export; }}\nstate cache {{ title \"Cache\"; description \"Local document encoding.\"; schema Document; }}\n}}\ndevice ReaderDevice {{ agent optional NativeReader {{ use documents {{ required capability read; }} }} }}");
    let graph = compile(&source).unwrap();
    let participant = graph
        .root_package()
        .participants()
        .values()
        .find(|participant| participant.name() == "Reader")
        .unwrap();
    let selection = participant.uses().values().next().unwrap();
    assert_eq!(
        selection
            .actions
            .iter()
            .map(|action| action.action.name.as_str())
            .collect::<Vec<_>>(),
        ["Export", "Get"]
    );
    let needs = graph.participant_needs(participant.identity()).unwrap();
    assert_eq!(needs.optional_action_capabilities().len(), 1);
    assert_eq!(
        needs
            .optional_action_capabilities()
            .keys()
            .next()
            .unwrap()
            .1
            .action
            .name,
        "Export"
    );
    let canonical = canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap();
    assert_eq!(
        compile(&canonical).unwrap().root_digest(),
        graph.root_digest()
    );
}

#[test]
fn duplicate_requests_normalize_and_required_selection_wins() {
    let required = SOURCE.replace("capability export;", "required capability export;");
    let duplicated = SOURCE.replace(
        "required capability read; capability export;",
        "capability read; required capability export; required capability read; capability export; required capability read;",
    );
    assert_eq!(
        compile(&required).unwrap().root_digest(),
        compile(&duplicated).unwrap().root_digest()
    );
}

#[test]
fn application_requests_refuse_private_resource_ownership() {
    let source = format!("{SOURCE}\napp ReaderApp {{ use documents {{ required capability read; }} kv cache {{ title \"Cache\"; description \"Private cache.\"; schema Document; }} }}");
    assert!(compile(&source).is_err());
}

#[test]
fn unknown_capability_and_nonpositive_consent_revision_are_refused() {
    assert!(
        compile(&SOURCE.replace("required capability read;", "required capability missing;"))
            .is_err()
    );
    assert!(compile(&SOURCE.replace("consent_revision 2;", "consent_revision 0;")).is_err());
}

#[test]
fn compatibility_refuses_consent_rollback_but_allows_editorial_changes() {
    let original = compile(SOURCE).unwrap();
    let lowered = compile(&SOURCE.replace("consent_revision 2;", "consent_revision 1;")).unwrap();
    let edited = compile(&SOURCE.replace("Read a document.", "Read document contents.")).unwrap();
    let api = original.root_package().apis().keys().next().unwrap();
    assert!(
        !compare_implementation(original.api(api).unwrap(), lowered.api(api).unwrap()).compatible
    );
    assert!(
        compare_implementation(original.api(api).unwrap(), edited.api(api).unwrap()).compatible
    );
}

#[test]
fn built_in_read_only_requests_select_only_the_requested_operations() {
    let sources = [
        (
            "auth_types",
            include_str!("../../runtime/src/builtin_source/auth_types.trellis"),
        ),
        (
            "types",
            include_str!("../../runtime/src/builtin_source/types.trellis"),
        ),
        (
            "auth",
            include_str!("../../runtime/src/builtin_source/auth.trellis"),
        ),
        (
            "state",
            include_str!("../../runtime/src/builtin_source/state.trellis"),
        ),
        (
            "jobs",
            include_str!("../../runtime/src/builtin_source/jobs.trellis"),
        ),
        (
            "health",
            include_str!("../../runtime/src/builtin_source/health.trellis"),
        ),
        (
            "events",
            include_str!("../../runtime/src/builtin_source/events.trellis"),
        ),
        (
            "core",
            include_str!("../../runtime/src/builtin_source/core.trellis"),
        ),
        (
            "reader",
            r#"
import { state } from state;
import { jobs } from jobs;
import { health } from health;
import { events } from events;
import { core } from core;
import { auth } from auth;
app Reader {
  use auth { required capability capabilitiesRead; }
  use state { required capability read; }
  use jobs { required capability query; }
  use health { required capability query; }
  use events { required capability query; }
  use core { required capability authority_read; }
}
"#,
        ),
    ];
    let graph = compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: "trellis".into(),
                version: Version::new(1, 0, 0),
            },
            sources: sources
                .iter()
                .map(|(alias, _)| (alias.to_string(), format!("{alias}.trellis")))
                .collect(),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        },
        sources
            .iter()
            .map(|(alias, source)| SourceUnit {
                alias: alias.to_string(),
                path: PathBuf::from(format!("{alias}.trellis")),
                source: source.to_string(),
            })
            .collect(),
        BTreeMap::new(),
    )
    .unwrap();
    let request = graph.root_package().app_requests().values().next().unwrap();
    for selection in request.uses().values() {
        let expected = match selection.api.as_str() {
            "trellis.auth@v1" => "Capabilities.List",
            "trellis.state@v1" => "Get",
            "trellis.core@v1" => "Surface.Status",
            _ => "Query",
        };
        assert_eq!(
            selection
                .actions
                .iter()
                .map(|action| action.action.name.as_str())
                .collect::<Vec<_>>(),
            [expected]
        );
    }
}
