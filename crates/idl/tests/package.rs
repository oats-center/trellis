//! Native format-2 package graph acceptance tests.

use semver::Version;
use std::{collections::BTreeMap, path::PathBuf};
use trellis_idl::project::{
    load_sources, read_manifest, Dependency, GenerateConfig, LockedDependency, LockedPackage,
    LockedSource, PackageLock, PackageManifest, PackageMetadata,
};
use trellis_idl::{
    canonical_package, compare_implementation, compare_resource, compare_resource_evolution,
    compare_selected, compile_evidence, compile_project, json_schema, CanonicalMode,
    PackageEvidence, PackageSourceEvidence, RetainedResourceActual, SelectedCompatibilityCache,
    SourceUnit, TypeDefinition,
};

fn manifest(sources: &[(&str, &str)]) -> PackageManifest {
    PackageManifest {
        package: PackageMetadata {
            name: "orders".into(),
            version: Version::new(1, 2, 3),
        },
        sources: sources
            .iter()
            .map(|(alias, path)| ((*alias).into(), (*path).into()))
            .collect(),
        dependencies: BTreeMap::new(),
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    }
}

fn source(alias: &str, path: &str, source: &str) -> SourceUnit {
    SourceUnit {
        alias: alias.into(),
        path: PathBuf::from(path),
        source: source.into(),
    }
}

#[test]
fn rejects_api_names_that_produce_invalid_api_ids() {
    let error = compile_project(
        &manifest(&[("api", "api.trellis")]),
        vec![source(
            "api",
            "api.trellis",
            r#"api Orders@v1 { title "Orders"; description "Orders."; capabilities { public {} } }"#,
        )],
        BTreeMap::new(),
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("invalid api id: must use lowercase alphanumeric lineage tokens"));
}

const TYPES: &str = r#"
model OrderId { value: ulid; }
model Empty {}
type OrderNote = string(min_length=1, max_length=200);
model Order { id: OrderId; note?: OrderNote; parent?: Order; }
model EventEnvelope { order: OrderId; }
type OrderList = list<Order>(min_items=0, max_items=100);
model OrderPage { items: OrderList; }
model GetInput { id: OrderId; page?: CursorQuery; }
type PagedOrders = CursorPage<Order>;
"#;

const API: &str = r#"
import { OrderId, Empty, EventEnvelope, GetInput, PagedOrders } from types;
api orders@v1 {
  title "Orders";
  description "Order access.";
  version "1.2.3";
  error Missing(OrderId);
  rpc Get {
    input GetInput;
    output PagedOrders;
    errors [Missing];
    pagination cursor;
  }
  event Changed {
    payload EventEnvelope;
    params [order.value];
  }
  capabilities {
    public {
      consent_revision 1;
      allows { publish event Changed; subscribe event Changed; }
    }
    capability read {
      consent_revision 1;
      title "Read orders";
      description "Reads orders.";
      consequence "Order data is shared.";
      allows { rpc Get; }
    }
  }
}
service backend {
  implements orders;
}
"#;

const RESOURCES: &str = r#"
import { OrderId, Empty } from types;
import { orders } from api;
service Worker {
  implements orders;
  job reconcile {
    title "Reconcile";
    description "Reconciles one order.";
    payload OrderId;
    result Empty;
  }
  consumer changes {
    title "Changes";
    description "Processes order changes.";
    events [orders.Changed];
    concurrency 2;
    replay all;
  }
}
device Sensor {
  kv optional cache {
    title "Cache";
    description "Local cache.";
    schema OrderId;
    history 2;
    ttl 5m;
    desired_max_value 1MiB;
  }
  app optional Operator {
    state preferences {
      title "Preferences";
      description "User preferences.";
      schema Empty;
    }
  }
}
"#;

#[test]
fn resolves_explicit_imports_into_typed_package_graph() {
    let graph = compile_project(
        &manifest(&[("types", "types.trellis"), ("api", "api.trellis")]),
        vec![
            source("api", "api.trellis", API),
            source("types", "types.trellis", TYPES),
        ],
        BTreeMap::new(),
    )
    .unwrap();

    let package = graph.root_package();
    assert_eq!(package.apis().len(), 1);
    assert_eq!(package.participants().len(), 1);
    let api = package.apis().values().next().unwrap();
    assert_eq!(api.identity().as_str(), "orders.orders@v1");
    let action = api.actions().values().next().unwrap();
    let trellis_idl::ActionDefinition::Rpc { input, .. } = action else {
        panic!("expected RPC")
    };
    assert_eq!(input.package.as_str(), "orders");
    assert_eq!(input.id.as_str(), "GetInput");
}

#[test]
fn canonical_source_is_stable_and_evidence_recompiles() {
    let package_manifest = manifest(&[("types", "types.trellis"), ("api", "api.trellis")]);
    let graph = compile_project(
        &package_manifest,
        vec![
            source("types", "types.trellis", TYPES),
            source("api", "api.trellis", API),
        ],
        BTreeMap::new(),
    )
    .unwrap();
    let canonical = canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap();
    let evidence = PackageEvidence {
        root_package: "orders".into(),
        root_digest: graph.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: "orders".into(),
            version: Version::new(1, 2, 3),
            digest: graph.root_digest().into(),
            source: canonical.clone(),
        }],
    };
    let mut noncanonical = evidence.clone();
    noncanonical.packages[0].source.insert(0, '\n');
    assert!(compile_evidence(noncanonical).is_err());
    let verified = compile_evidence(evidence).unwrap();

    assert_eq!(verified.root_digest(), graph.root_digest());
    assert_eq!(
        canonical_package(&verified, verified.root(), CanonicalMode::Presentation).unwrap(),
        canonical
    );
    let semantic = canonical_package(&graph, graph.root(), CanonicalMode::Semantic).unwrap();
    let semantic_round_trip = compile_project(
        &manifest(&[("canonical", "canonical.trellis")]),
        vec![source("canonical", "canonical.trellis", &semantic)],
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(semantic_round_trip.root_digest(), graph.root_digest());

    let moved = compile_project(
        &manifest(&[
            ("schema", "schema.trellis"),
            ("contract", "contract.trellis"),
        ]),
        vec![
            source("schema", "schema.trellis", TYPES),
            source(
                "contract",
                "contract.trellis",
                &API.replace("from types", "from schema"),
            ),
        ],
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(moved.root_digest(), graph.root_digest());
}

#[test]
fn participant_digest_ignores_unrelated_package_declarations() {
    let original = compile_project(
        &manifest(&[("participants", "participants.trellis")]),
        vec![source(
            "participants",
            "participants.trellis",
            "service Caller {}\n",
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let changed = compile_project(
        &manifest(&[("participants", "participants.trellis")]),
        vec![source(
            "participants",
            "participants.trellis",
            "service Caller {}\napp Unrelated {}\n",
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let participant = original
        .root_package()
        .participants()
        .keys()
        .find(|id| id.as_str() == "orders.Caller")
        .unwrap();
    assert_eq!(
        trellis_idl::participant_digest(&original, participant).unwrap(),
        trellis_idl::participant_digest(&changed, participant).unwrap()
    );
    let participant_changed = compile_project(
        &manifest(&[("participants", "participants.trellis")]),
        vec![source(
            "participants",
            "participants.trellis",
            "device Caller {}\n",
        )],
        BTreeMap::new(),
    )
    .unwrap();
    assert_ne!(
        trellis_idl::participant_digest(&original, participant).unwrap(),
        trellis_idl::participant_digest(&participant_changed, participant).unwrap()
    );
}

#[test]
fn projects_open_models_and_exact_wire_scalars() {
    let graph = compile_project(
        &manifest(&[("types", "types.trellis")]),
        vec![source(
            "types",
            "types.trellis",
            r#"
model OrderId { value: ulid; }
type OrderNote = string(min_length=1, max_length=200);
model Order { id: OrderId; note?: OrderNote; }
"#,
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let order = graph
        .root_package()
        .types()
        .keys()
        .find(|id| id.as_str() == "Order")
        .unwrap();
    let schema = json_schema(
        &graph,
        &trellis_idl::TypeRef {
            package: graph.root().clone(),
            id: order.clone(),
        },
    )
    .unwrap();

    assert_eq!(
        schema["$defs"]["orders.Order"]["additionalProperties"],
        true
    );
    assert_eq!(
        schema["$defs"]["orders.OrderId"]["properties"]["value"]["pattern"],
        "^[0-7][0-9A-HJKMNP-TV-Z]{25}$"
    );
}

#[test]
fn reports_source_span_for_unimported_names() {
    let error = compile_project(
        &manifest(&[("api", "api.trellis"), ("types", "types.trellis")]),
        vec![
            source("types", "types.trellis", "model Hidden {}"),
            source("api", "api.trellis", "type Alias = Hidden;"),
        ],
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("not declared in this file"));
    assert!(error
        .labels()
        .is_some_and(|mut labels| labels.next().is_some()));
}

#[test]
fn local_imports_require_a_type_or_api_in_the_named_source() {
    let package_manifest = manifest(&[
        ("types", "types.trellis"),
        ("other", "other.trellis"),
        ("consumer", "consumer.trellis"),
    ]);
    for import in [
        "import { Hidden } from other;",
        "import { Caller } from types;",
    ] {
        let error = compile_project(
            &package_manifest,
            vec![
                source("types", "types.trellis", "model Hidden {} app Caller {}"),
                source("other", "other.trellis", "model Other {}"),
                source("consumer", "consumer.trellis", import),
            ],
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("refers to missing type or API"));
        assert!(error
            .labels()
            .is_some_and(|mut labels| labels.next().is_some()));
    }
}

#[test]
fn shared_schema_changes_update_only_affected_participant_needs() {
    let package_manifest = manifest(&[
        ("types", "types.trellis"),
        ("api", "api.trellis"),
        ("app", "app.trellis"),
    ]);
    let api = API.replace(
        "  event Changed {",
        "  rpc Again { input GetInput; output PagedOrders; errors [Missing]; pagination cursor; }\n  event Changed {",
    ).replace(
        "    capability read {",
        "    capability again { title \"Read again\"; description \"Read another page.\"; consequence \"Order data is shared.\"; consent_revision 1; allows { rpc Again; } }\n    capability read {",
    );
    let compile = |types: &str| {
        compile_project(
            &package_manifest,
            vec![
                source("types", "types.trellis", types),
                source("api", "api.trellis", &api),
                source(
                    "app",
                    "app.trellis",
                    r#"
import { orders } from api;
service First { use orders { required capability read; required capability again; } }
service Second { use orders { required capability read; } }
service Observer { use orders { required capability public; } }
"#,
                ),
            ],
            BTreeMap::new(),
        )
        .unwrap()
    };
    let original = compile(TYPES);
    let changed = compile(&TYPES.replace("max_length=200", "max_length=201"));
    let verified = compile_evidence(PackageEvidence {
        root_package: package_manifest.package.name.clone(),
        root_digest: changed.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: package_manifest.package.name.clone(),
            version: package_manifest.package.version.clone(),
            digest: changed.root_digest().into(),
            source: canonical_package(&changed, changed.root(), CanonicalMode::Presentation)
                .unwrap(),
        }],
    })
    .unwrap();
    for participant in original.root_package().participants().values() {
        let id = participant.identity();
        let before = original.participant_needs(id).unwrap().digest();
        let after = changed.participant_needs(id).unwrap().digest();
        if participant.name() == "Observer" {
            assert_eq!(before, after);
        } else {
            assert_ne!(before, after, "{}", participant.name());
        }
        assert_eq!(after, verified.participant_needs(id).unwrap().digest());
    }
}

#[test]
fn semantic_digest_excludes_titles_but_includes_capability_consent_text() {
    let compile = |api: &str| {
        compile_project(
            &manifest(&[("types", "types.trellis"), ("api", "api.trellis")]),
            vec![
                source("types", "types.trellis", TYPES),
                source("api", "api.trellis", api),
            ],
            BTreeMap::new(),
        )
        .unwrap()
    };
    let original = compile(API);
    let titles = compile(
        &API.replace("title \"Orders\"", "title \"Order catalog\"")
            .replace("title \"Read orders\"", "title \"Browse\""),
    );
    let consent = compile(&API.replace("Reads orders.", "Reads current orders."));

    assert_eq!(original.root_digest(), titles.root_digest());
    assert_ne!(original.root_digest(), consent.root_digest());
}

#[test]
fn marks_only_direct_recursive_model_edges() {
    let graph = compile_project(
        &manifest(&[("types", "types.trellis")]),
        vec![source(
            "types",
            "types.trellis",
            "model Node { next?: Node; children: list<Node>; } model Left { right: Right; } model Right { left: Left; }",
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let TypeDefinition::Model(fields) = graph
        .root_package()
        .types()
        .iter()
        .find(|(id, _)| id.as_str() == "Node")
        .unwrap()
        .1
    else {
        panic!("expected model")
    };

    assert!(fields["next"].recursive);
    assert!(!fields["children"].recursive);
    for name in ["Left", "Right"] {
        let TypeDefinition::Model(fields) = graph
            .root_package()
            .types()
            .iter()
            .find(|(id, _)| id.as_str() == name)
            .unwrap()
            .1
        else {
            panic!("expected model")
        };
        assert!(fields.values().all(|field| field.recursive));
    }
}

#[test]
fn resource_and_companion_evidence_recompiles_without_changing_meaning() {
    let trellis_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../runtime");
    let trellis_manifest = read_manifest(&trellis_root.join("trellis.toml")).unwrap();
    let trellis = compile_project(
        &trellis_manifest,
        load_sources(&trellis_root, &trellis_manifest).unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let mut manifest = manifest(&[
        ("types", "types.trellis"),
        ("api", "api.trellis"),
        ("resources", "resources.trellis"),
    ]);
    manifest.dependencies.insert(
        "platform".into(),
        Dependency {
            package: "trellis".into(),
            version: None,
            path: Some("../runtime".into()),
            registry: None,
        },
    );
    let graph = compile_project(
        &manifest,
        vec![
            source("types", "types.trellis", TYPES),
            source("api", "api.trellis", API),
            source("resources", "resources.trellis", RESOURCES),
        ],
        BTreeMap::from([("platform".into(), trellis.clone())]),
    )
    .unwrap();

    let presentation =
        canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap();
    let round_trip = compile_project(
        &PackageManifest {
            sources: BTreeMap::from([("canonical".into(), "canonical.trellis".into())]),
            dependencies: BTreeMap::from([(
                "d0".into(),
                manifest.dependencies["platform"].clone(),
            )]),
            ..manifest
        },
        vec![source("canonical", "canonical.trellis", &presentation)],
        BTreeMap::from([("d0".into(), trellis)]),
    )
    .unwrap();
    assert_eq!(round_trip.root_digest(), graph.root_digest());
}

#[test]
fn consumer_requires_explicit_trellis_events_dependency() {
    let error = compile_project(
        &manifest(&[("api", "api.trellis")]),
        vec![source(
            "api",
            "api.trellis",
            r#"model Value {}
api orders@v1 {
  title "Orders"; description "Orders";
  event Changed { payload Value; }
  capabilities { public { consent_revision 1; allows { publish event Changed; subscribe event Changed; } } }
}
service Worker {
  consumer changes {
    title "Changes"; description "Processes changes";
    events [orders.Changed]; concurrency 1; replay new;
  }
}"#,
        )],
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("trellis.events@v1"), "{error}");
}

#[test]
fn preserves_integer_bounds_without_floating_point_rounding() {
    let graph = compile_project(
        &manifest(&[("types", "types.trellis")]),
        vec![source(
            "types",
            "types.trellis",
            "type Big = uint64(min=9007199254740993, max=18446744073709551615);",
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let canonical = canonical_package(&graph, graph.root(), CanonicalMode::Semantic).unwrap();
    assert!(canonical.contains("min=9007199254740993, max=18446744073709551615"));

    assert!(compile_project(
        &manifest(&[("types", "types.trellis")]),
        vec![source(
            "types",
            "types.trellis",
            "type TooBig = uint64(max=18446744073709551616);",
        )],
        BTreeMap::new(),
    )
    .is_err());
}

#[test]
fn format_two_lock_rejects_cycles_and_inexact_edges() {
    let digest = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let distribution = format!("sha256:{}", "0".repeat(64));
    let mut lock = PackageLock {
        format: 2,
        root: "a".into(),
        packages: vec![
            LockedPackage {
                name: "a".into(),
                version: Version::new(1, 0, 0),
                digest: digest.into(),
                distribution_digest: distribution.clone(),
                dependencies: vec![LockedDependency {
                    name: "b".into(),
                    version: Version::new(1, 0, 0),
                    digest: digest.into(),
                }],
                source: LockedSource::Path { path: "a".into() },
            },
            LockedPackage {
                name: "b".into(),
                version: Version::new(1, 0, 0),
                digest: digest.into(),
                distribution_digest: distribution.clone(),
                dependencies: vec![],
                source: LockedSource::Path { path: "b".into() },
            },
        ],
    };
    assert!(lock.validate().is_ok());

    lock.packages[1].dependencies.push(LockedDependency {
        name: "a".into(),
        version: Version::new(1, 0, 0),
        digest: digest.into(),
    });
    assert!(lock.validate().is_err());

    lock.packages[1].dependencies.clear();
    lock.packages.push(LockedPackage {
        name: "c".into(),
        version: Version::new(1, 0, 0),
        digest: digest.into(),
        distribution_digest: distribution,
        dependencies: vec![],
        source: LockedSource::Path { path: "c".into() },
    });
    assert!(lock.validate().is_err());
}

#[test]
fn selected_compatibility_is_directional_and_recursive() {
    const APP: &str = r#"
    import { orders } from api;
    app Shopper { use orders { required capability read; } }
"#;
    let package_manifest = manifest(&[
        ("types", "types.trellis"),
        ("api", "api.trellis"),
        ("app", "app.trellis"),
    ]);
    let compile = |types: &str| {
        compile_project(
            &package_manifest,
            vec![
                source("types", "types.trellis", types),
                source("api", "api.trellis", API),
                source("app", "app.trellis", APP),
            ],
            BTreeMap::new(),
        )
        .unwrap()
    };
    let consumer = compile(TYPES);
    let provider = compile(&TYPES.replace(
        "model GetInput { id: OrderId; page?: CursorQuery; }",
        "model GetInput { id: OrderId; page?: CursorQuery; tenant: string; }",
    ));
    let selection = consumer
        .root_package()
        .app_requests()
        .values()
        .find(|participant| participant.name() == "Shopper")
        .unwrap()
        .uses()
        .values()
        .next()
        .unwrap();

    let report = compare_selected(&consumer, selection, &provider);
    assert!(!report.compatible);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.path.contains(".input.field.tenant")));
    let missing_action_provider = compile_project(
        &manifest(&[("types", "types.trellis"), ("api", "api.trellis")]),
        vec![
            source("types", "types.trellis", TYPES),
            source("api", "api.trellis", &API.replace("rpc Get", "rpc Other")),
        ],
        BTreeMap::new(),
    )
    .unwrap();
    let report = compare_selected(&consumer, selection, &missing_action_provider);
    assert!(!report.compatible);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.message == "selected provider action is unavailable"));
    let mut cache = SelectedCompatibilityCache::default();
    assert!(
        !cache
            .compare(&consumer, selection, &provider)
            .unwrap()
            .compatible
    );
    assert!(
        !cache
            .compare(&consumer, selection, &provider)
            .unwrap()
            .compatible
    );
    assert_eq!(cache.len(), 1);
    let event_selection = trellis_idl::InteractionSelection {
        api: selection.api.clone(),
        actions: std::collections::BTreeSet::from([trellis_idl::ActionSelection {
            action: trellis_idl::ActionId {
                kind: trellis_idl::ActionKind::Event,
                name: "Changed".into(),
            },
            direction: trellis_idl::InteractionDirection::Subscribe,
        }]),
        optional_capabilities: std::collections::BTreeSet::new(),
        required_capabilities: std::collections::BTreeSet::new(),
    };
    assert!(
        cache
            .compare(&consumer, &event_selection, &provider)
            .unwrap()
            .compatible
    );
    assert_eq!(cache.len(), 2);
    assert!(
        cache
            .compare(&consumer, selection, &consumer)
            .unwrap()
            .compatible
    );
    assert_eq!(cache.len(), 3);

    let api = consumer.root_package().apis().keys().next().unwrap();
    let report = compare_implementation(consumer.api(api).unwrap(), provider.api(api).unwrap());
    assert!(!report.compatible);
}

#[test]
fn constraints_require_named_aliases() {
    let manifest = manifest(&[("types", "types.trellis")]);
    assert!(compile_project(
        &manifest,
        vec![source(
            "types",
            "types.trellis",
            "model Invalid { value: string(min_length=1); }",
        )],
        BTreeMap::new(),
    )
    .is_err());
    assert!(compile_project(
        &manifest,
        vec![source(
            "types",
            "types.trellis",
            "type Invalid = list<string(min_length=1)>(min_items=1);",
        )],
        BTreeMap::new(),
    )
    .is_err());
}

#[test]
fn rejects_invalid_capability_participant_and_pagination_placement() {
    let compile = |text: &str| {
        compile_project(
            &manifest(&[("source", "source.trellis")]),
            vec![source("source", "source.trellis", text)],
            BTreeMap::new(),
        )
    };
    assert!(compile(
        r#"
model Empty {}
api public@v1 { title "Public"; description "Public."; capabilities {} }
app Invalid { implements public; }
"#
    )
    .is_err());
    assert!(compile(
        r#"
model Empty {}
api public@v1 { title "Public"; description "Public."; capabilities {} }
service Valid { use public; }
"#
    )
    .is_ok());
    assert!(compile("model NotARequest { page?: CursorQuery; }").is_err());
    assert!(compile("type NotAResponse = CursorPage<string>;").is_err());
}

#[test]
fn dependency_and_source_aliases_do_not_affect_digest() {
    let dependency_manifest = PackageManifest {
        package: PackageMetadata {
            name: "common".into(),
            version: Version::new(1, 0, 0),
        },
        sources: BTreeMap::from([("source".into(), "common.trellis".into())]),
        dependencies: BTreeMap::new(),
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    };
    let dependency = compile_project(
        &dependency_manifest,
        vec![source(
            "source",
            "common.trellis",
            r#"
model Empty {}
model Shared { value: string; }
model Hidden { value: string; }
api common@v1 {
  title "Common";
  description "Common values.";
  rpc Get { input Empty; output Shared; }
    capabilities { public { consent_revision 1; allows { rpc Get; } } }
}
"#,
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let root = |source_alias: &str, path: &str, dependency_alias: &str| PackageManifest {
        package: PackageMetadata {
            name: "orders".into(),
            version: Version::new(1, 2, 3),
        },
        sources: BTreeMap::from([(source_alias.into(), path.into())]),
        dependencies: BTreeMap::from([(
            dependency_alias.into(),
            Dependency {
                package: "common".into(),
                version: None,
                path: Some("common".into()),
                registry: None,
            },
        )]),
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    };
    let compile = |source_alias: &str, path: &str, dependency_alias: &str| {
        compile_project(
            &root(source_alias, path, dependency_alias),
            vec![source(
                source_alias,
                path,
                &format!(
                    "import {{ Shared }} from {dependency_alias};\ntype SharedValue = Shared;\n"
                ),
            )],
            BTreeMap::from([(dependency_alias.into(), dependency.clone())]),
        )
        .unwrap()
    };
    let graph = compile("root", "root.trellis", "common");
    assert_eq!(
        graph.root_digest(),
        compile("moved", "src/moved.trellis", "shared").root_digest()
    );
    let evidence = PackageEvidence {
        root_package: "orders".into(),
        root_digest: graph.root_digest().into(),
        packages: vec![
            PackageSourceEvidence {
                name: "common".into(),
                version: Version::new(1, 0, 0),
                digest: dependency.root_digest().into(),
                source: canonical_package(
                    &dependency,
                    dependency.root(),
                    CanonicalMode::Presentation,
                )
                .unwrap(),
            },
            PackageSourceEvidence {
                name: "orders".into(),
                version: Version::new(1, 2, 3),
                digest: graph.root_digest().into(),
                source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation)
                    .unwrap(),
            },
        ],
    };
    assert_eq!(
        compile_evidence(evidence.clone()).unwrap().root_digest(),
        graph.root_digest()
    );
    let mut incomplete = evidence.clone();
    incomplete.packages.remove(0);
    assert!(compile_evidence(incomplete).is_err());
    let mut overcomplete = evidence;
    overcomplete.packages.push(PackageSourceEvidence {
        name: "unused".into(),
        version: Version::new(1, 0, 0),
        digest: "unused".into(),
        source: "unused".into(),
    });
    assert!(compile_evidence(overcomplete).is_err());

    let error = compile_project(
        &root("root", "root.trellis", "common"),
        vec![source(
            "root",
            "root.trellis",
            "import { Hidden } from common;\nmodel Local {}\n",
        )],
        BTreeMap::from([("common".into(), dependency)]),
    )
    .unwrap_err();
    let diagnostic = format!("{error:?}");
    assert!(diagnostic.contains("root.trellis"));
    assert!(diagnostic.contains("Hidden"));
}

#[test]
fn resource_compatibility_uses_schema_revisions_and_retains_history() {
    let manifest = manifest(&[("types", "types.trellis"), ("app", "app.trellis")]);
    let compile = |types: &str, body: &str| {
        compile_project(
            &manifest,
            vec![
                source("types", "types.trellis", types),
                source(
                    "app",
                    "app.trellis",
                    &format!(
                        "import {{ V1, V2 }} from types; device Client {{ state value {{ title \"Value\"; description \"Stored value.\"; {body} }} }}"
                    ),
                ),
            ],
            BTreeMap::new(),
        )
        .unwrap()
    };
    let previous = compile(
        "type V1 = string; type V2 = string;",
        "schema V1; version 1;",
    );
    let same_version = compile(
        "type V1 = string(min_length=1); type V2 = string;",
        "schema V1; version 1;",
    );
    let replacement = compile(
        "type V1 = string; type V2 = string(min_length=1);",
        "schema V2; version 2; accepts { 1: V1; }",
    );
    let participant = previous
        .root_package()
        .participants()
        .keys()
        .next()
        .unwrap();
    let resource = previous
        .root_package()
        .participants()
        .get(participant)
        .unwrap()
        .resources()
        .keys()
        .next()
        .unwrap();
    assert!(
        !compare_resource_evolution(
            previous.resource(participant, resource).unwrap(),
            same_version.resource(participant, resource).unwrap(),
        )
        .compatible
    );
    let report = compare_resource_evolution(
        previous.resource(participant, resource).unwrap(),
        replacement.resource(participant, resource).unwrap(),
    );
    assert!(report.compatible);
    assert!(report.migration_required);
}

#[test]
fn derives_operation_subjects_and_signal_codecs() {
    let graph = compile_project(
        &manifest(&[("source", "source.trellis")]),
        vec![source(
            "source",
            "source.trellis",
            r#"
model Empty {}
api work@v2 {
  title "Work";
  description "Work API.";
  operation Run { input Empty; output Empty; signals { stop Empty; } }
  capabilities {
    capability execute {
      consent_revision 1;
      title "Execute work";
      description "Runs work.";
      consequence "Work is executed.";
      allows { operation Run; }
    }
  }
}
service Client { use work { required capability execute; } }
service Worker { implements work; }
"#,
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let api = graph.root_package().apis().values().next().unwrap();
    assert_eq!(api.subjects().operations["Run"], "operations.v2.work.Run");
    let action = api.actions().keys().next().unwrap();
    let codecs = graph.action_codecs(api.identity(), action).unwrap();
    assert!(codecs.input.is_some());
    assert!(codecs.output.is_some());
    assert!(codecs.signals.contains_key("stop"));
}

#[test]
fn compares_resource_declarations_to_retained_commitments() {
    let graph = compile_project(
        &manifest(&[("source", "source.trellis")]),
        vec![source(
            "source",
            "source.trellis",
            r#"
model Value {}
device Sensor {
  kv optional cache {
    title "Cache";
    description "Cache.";
    schema Value;
    history 2;
    ttl 5m;
    desired_max_value 1MiB;
  }
}

"#,
        )],
        BTreeMap::new(),
    )
    .unwrap();
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
    assert!(compare_resource(resource, &RetainedResourceActual::Unavailable).compatible);
    assert!(
        compare_resource(
            resource,
            &RetainedResourceActual::Kv {
                history: 2,
                ttl_ms: 300_000,
                max_value_bytes: Some(1_048_576),
            }
        )
        .compatible
    );
    assert!(
        !compare_resource(
            resource,
            &RetainedResourceActual::Kv {
                history: 1,
                ttl_ms: 60_000,
                max_value_bytes: Some(1024),
            }
        )
        .compatible
    );
}

#[test]
fn app_state_encoding_preserves_explicit_read_only_selection() {
    let state_manifest = PackageManifest {
        package: PackageMetadata {
            name: "trellis".into(),
            version: Version::new(1, 0, 0),
        },
        sources: BTreeMap::from([("state".into(), "state.trellis".into())]),
        dependencies: BTreeMap::new(),
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    };
    let state_graph = compile_project(
        &state_manifest,
        vec![source(
            "state",
            "state.trellis",
            r#"
model Empty {}
api state@v1 {
  title "State"; description "State runtime.";
  rpc Get { input Empty; output Empty; }
  rpc Put { input Empty; output Empty; }
  rpc Delete { input Empty; output Empty; }
  capabilities {
    capability read {
      title "Read"; description "Read a State value.";
      consequence "The caller sees saved values.";
      consent_revision 1; allows { rpc Get; }
    }
    capability write {
      title "Write"; description "Overwrite a State value.";
      consequence "The caller replaces saved values.";
      consent_revision 1; allows { rpc Put; }
    }
    capability delete {
      title "Delete"; description "Delete a State value.";
      consequence "The caller removes saved values.";
      consent_revision 1; allows { rpc Delete; }
    }
  }
}
"#,
        )],
        BTreeMap::new(),
    )
    .unwrap();
    let mut app_manifest = manifest(&[("source", "source.trellis")]);
    app_manifest.dependencies.insert(
        "trellis".into(),
        Dependency {
            package: "trellis".into(),
            version: None,
            path: Some("../trellis".into()),
            registry: None,
        },
    );
    let graph = compile_project(
        &app_manifest,
        vec![source("source", "source.trellis", "import { state } from trellis;\nmodel Saved { value: string; }\napp Client { use state { required capability read; } state saved { title \"Saved\"; description \"Saved value.\"; schema Saved; } }")],
        BTreeMap::from([("trellis".into(), state_graph)]),
    )
    .unwrap();

    let client = graph
        .root_package()
        .app_requests()
        .keys()
        .find(|id| id.as_str().ends_with(".Client"))
        .unwrap();
    let request = &graph.root_package().app_requests()[client];
    let state_use = request
        .uses()
        .values()
        .find(|selection| selection.api.as_str() == "trellis.state@v1")
        .unwrap();
    assert_eq!(
        state_use
            .actions
            .iter()
            .map(|selection| selection.action.name.as_str())
            .collect::<Vec<_>>(),
        ["Get"]
    );
}

const OPERATION_SOURCE: &str = r#"
model Status { stage: string; }
model Preview { text: string; }
model Empty {}
api work@v1 {
  title "Work";
  description "Work operations.";
  operation Run { input Empty; output Empty; progress Status; update Preview; }
  capabilities { public { consent_revision 1; allows { operation Run; } } }
}
service Worker { implements work; }
"#;

fn compile_operation(text: &str) -> trellis_idl::PackageGraph {
    compile_project(
        &manifest(&[("work", "work.trellis")]),
        vec![source("work", "work.trellis", text)],
        BTreeMap::new(),
    )
    .unwrap()
}

/// The parser accepts both members and each channel keeps its own schema.
#[test]
fn operation_declares_distinct_progress_and_update_schemas() {
    let graph = compile_operation(OPERATION_SOURCE);
    let api = graph.root_package().apis().values().next().unwrap();
    let action = api.actions().keys().next().unwrap();
    let codecs = graph.action_codecs(api.identity(), action).unwrap();
    let progress = codecs.progress.expect("progress schema");
    let update = codecs.update.expect("update schema");
    assert_ne!(progress, update, "channels keep distinct schemas");
    assert_eq!(
        progress.get("$ref").and_then(serde_json::Value::as_str),
        Some("#/$defs/orders.Status")
    );
    assert_eq!(
        update.get("$ref").and_then(serde_json::Value::as_str),
        Some("#/$defs/orders.Preview")
    );

    let canonical = canonical_package(&graph, graph.root(), CanonicalMode::Semantic).unwrap();
    assert!(canonical.contains("progress Status;"));
    assert!(canonical.contains("update Preview;"));
    // Canonical output recompiles unchanged.
    let recompiled = compile_operation(&canonical);
    assert_eq!(
        canonical_package(&recompiled, recompiled.root(), CanonicalMode::Semantic).unwrap(),
        canonical
    );
}

/// A progress-only operation keeps its previous shape, reuses the progress
/// schema for live updates, and adding an explicit update schema is a change.
#[test]
fn operation_update_is_optional_and_extends_the_contract() {
    let progress_only = OPERATION_SOURCE.replace(" update Preview;", "");
    let graph = compile_operation(&progress_only);
    let api = graph.root_package().apis().values().next().unwrap();
    let action = api.actions().keys().next().unwrap();
    let codecs = graph.action_codecs(api.identity(), action).unwrap();
    let progress = codecs.progress.clone().expect("progress schema");
    assert_eq!(
        codecs.update.expect("live fallback schema"),
        progress,
        "no dedicated update schema reuses progress"
    );
    let canonical = canonical_package(&graph, graph.root(), CanonicalMode::Semantic).unwrap();
    assert!(canonical.contains("progress Status;"));
    assert!(!canonical.contains("update Preview;"));

    let with_update = compile_operation(OPERATION_SOURCE);
    let previous = graph.api(api.identity()).unwrap();
    let replacement = with_update.api(api.identity()).unwrap();
    let report = compare_implementation(previous, replacement);
    assert!(
        !report.compatible,
        "an explicit update schema changes the live channel"
    );
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.path.contains(".update")));
    let repeated = compile_operation(OPERATION_SOURCE);
    let identical = repeated.api(api.identity()).unwrap();
    let replacement = with_update.api(api.identity()).unwrap();
    assert!(compare_implementation(identical, replacement).compatible);
}

/// The live update channel compares its effective schema (explicit update or
/// progress fallback), not the raw presence of the `update` member.
#[test]
fn operation_live_channel_compares_effective_schema() {
    let progress_only = compile_operation(&OPERATION_SOURCE.replace(" update Preview;", ""));
    let explicit_same =
        compile_operation(&OPERATION_SOURCE.replace("update Preview;", "update Status;"));
    let explicit_preview = compile_operation(OPERATION_SOURCE);
    let api = progress_only
        .root_package()
        .apis()
        .values()
        .next()
        .unwrap()
        .identity()
        .clone();

    // Declaring an explicit update identical to the progress fallback adds no
    // effective live-schema change.
    let report = compare_implementation(
        progress_only.api(&api).unwrap(),
        explicit_same.api(&api).unwrap(),
    );
    assert!(report.compatible, "{:?}", report.issues);

    // Removing an explicit update that differed from progress changes the live schema.
    let report = compare_implementation(
        explicit_preview.api(&api).unwrap(),
        progress_only.api(&api).unwrap(),
    );
    assert!(!report.compatible);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.path.contains(".update")));

    // Replacing the explicit update schema changes the live schema.
    let report = compare_implementation(
        explicit_same.api(&api).unwrap(),
        explicit_preview.api(&api).unwrap(),
    );
    assert!(!report.compatible);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.path.contains(".update")));

    // An unchanged declaration stays compatible.
    let repeated = compile_operation(OPERATION_SOURCE);
    assert!(
        compare_implementation(
            explicit_preview.api(&api).unwrap(),
            repeated.api(&api).unwrap(),
        )
        .compatible
    );
}
