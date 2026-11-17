use super::*;
use crate::platform::auth::packages::{held_evidence, resolve};
use crate::platform::auth::rpc_deployments::{apply_deployment, create_deployment};
use trellis_runtime_apis::__types::trellis::{
    AuthDeploymentApplyRequest, AuthDeploymentCreateRequest, AuthDeploymentKind,
    AuthDeploymentMutationResponse, AuthPackageEvidence, AuthPackageOrigin,
    AuthPackageResolveRequest, AuthPackageResolveResponse,
};

/// Ask Auth to build evidence from `request`, with every package it holds now.
fn resolve_with(f: &Fixture, request: serde_json::Value) -> AuthPackageResolveResponse {
    let request: AuthPackageResolveRequest = serde_json::from_value(request).unwrap();
    let held = f.store.transaction(held_evidence).unwrap();
    resolve(held, &request).unwrap()
}

/// One package's request entry: its trellis.toml and one source file.
fn package(manifest: &str, path: &str, source: &str) -> serde_json::Value {
    serde_json::json!({
        "manifest": manifest,
        "files": [{ "path": path, "text": source }],
    })
}

fn create(
    f: &Fixture,
    title: &str,
    participant: &str,
    evidence: AuthPackageEvidence,
) -> AuthDeploymentMutationResponse {
    let input = AuthDeploymentCreateRequest {
        acknowledge_shared_apis: None,
        package_evidence: evidence,
        participant_id: participant.into(),
        kind: AuthDeploymentKind::Service,
        title: title.to_string().into(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    create_deployment(&f.store, &mutation(&f.admin, &input), &input, NOW).unwrap()
}

#[test]
fn a_project_depending_on_trellis_resolves_to_the_evidence_the_cli_builds() {
    let f = fixture();
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../integration/fixtures/runtime")
        .canonicalize()
        .unwrap();
    let read = |name: &str| std::fs::read_to_string(project.join(name)).unwrap();
    let resolved = resolve_with(
        &f,
        serde_json::json!({
            "root": package(&read("trellis.toml"), "contract.trellis", &read("contract.trellis")),
        }),
    );
    assert!(resolved.missing.is_empty(), "{:?}", resolved.missing);
    let [trellis] = resolved.dependencies.as_slice() else {
        panic!("{:?}", resolved.dependencies)
    };
    assert_eq!(trellis.name.0, "trellis");
    assert_eq!(trellis.origin, AuthPackageOrigin::Builtin);
    assert!(resolved
        .participants
        .iter()
        .any(
            |participant| participant.participant_id == "runtime-trellis.LiveV1Provider"
                && participant.kind == AuthDeploymentKind::Service
        ));

    let built = trellis_cli::app::deploy::compile_participant_input(
        &project,
        Some("runtime-trellis.LiveV1Provider"),
        Some(trellis_idl::ParticipantKind::Service),
    )
    .unwrap();
    let evidence = resolved.package_evidence.unwrap();
    assert_eq!(evidence, built.package_evidence);

    let created = create(&f, "Live", "runtime-trellis.LiveV1Provider", evidence);
    assert_eq!(
        created.deployment.implementation_digest.0,
        built.participant_digest
    );
}

const INVENTORY: &str = r#"
model Item { sku: string; }
api inventory@v1 {
  title "Inventory";
  description "Stock levels.";
  rpc Get { input Item; output Item; }
  capabilities {
    capability inventory.read { title "Read stock"; description "Read stock levels."; grants "Sees stock levels."; allows { rpc Get; } }
  }
}
service Warehouse { implements inventory; }
"#;

fn inventory_manifest(version: &str) -> String {
    format!("[package]\nname = \"inventory\"\nversion = \"{version}\"\n\n[sources]\nmain = \"inventory.trellis\"\n")
}

fn shop_manifest(requirement: &str) -> String {
    format!(
        "[package]\nname = \"shop\"\nversion = \"1.0.0\"\n\n[sources]\nshop = \"src/shop.trellis\"\n\n\
        [dependencies]\ninventory = {{ package = \"inventory\", version = \"{requirement}\", registry = \"ghcr.io/example\" }}\n"
    )
}

const SHOP: &str = r#"
import { inventory } from inventory;
service Shop { use inventory { capability inventory.read; } }
"#;

#[test]
fn a_dependency_on_an_installed_service_package_takes_the_highest_matching_installed_version() {
    let f = fixture();
    let install = |version: &str, source: &str| {
        resolve_with(
            &f,
            serde_json::json!({
                "root": package(&inventory_manifest(version), "inventory.trellis", source),
            }),
        )
        .package_evidence
        .unwrap()
    };
    let first = install("1.0.0", INVENTORY);
    let warehouse = create(&f, "Warehouse", "inventory.Warehouse", first.clone());
    let newer = INVENTORY.replace(
        "rpc Get { input Item; output Item; }",
        "rpc Get { input Item; output Item; }\n  rpc Count { input Item; output Item; }",
    );
    let second = install("1.2.0", &newer);
    let apply = AuthDeploymentApplyRequest {
        acknowledge_shared_apis: None,
        deployment_id: warehouse.deployment.deployment_id,
        expected_revision: warehouse.deployment.revision,
        package_evidence: second.clone(),
        idempotency_key: id().into(),
        extra: Default::default(),
    };
    apply_deployment(&f.store, &mutation(&f.admin, &apply), &apply, NOW + 1).unwrap();

    // The file sits in a folder; its name alone finds it.
    let shop = |requirement: &str| {
        resolve_with(
            &f,
            serde_json::json!({ "root": package(&shop_manifest(requirement), "shop.trellis", SHOP) }),
        )
    };
    let highest = shop("^1.0.0");
    assert!(highest.missing.is_empty(), "{:?}", highest.missing);
    let [chosen] = highest.dependencies.as_slice() else {
        panic!("{:?}", highest.dependencies)
    };
    assert_eq!(
        (
            chosen.name.0.as_str(),
            chosen.version.0.as_str(),
            &chosen.origin
        ),
        ("inventory", "1.2.0", &AuthPackageOrigin::Installed)
    );
    assert_eq!(
        chosen
            .other_versions
            .iter()
            .map(|version| version.0.as_str())
            .collect::<Vec<_>>(),
        ["1.0.0"]
    );
    let evidence = highest.package_evidence.unwrap();
    let installed = second
        .packages
        .iter()
        .find(|package| package.name.0 == "inventory")
        .unwrap();
    assert!(evidence.packages.contains(installed));
    create(&f, "Shop", "shop.Shop", evidence);

    // The requirement decides, not recency.
    let pinned = shop("=1.0.0");
    assert_eq!(pinned.dependencies[0].version.0, "1.0.0");
    assert!(pinned
        .package_evidence
        .unwrap()
        .packages
        .iter()
        .any(|package| first.packages.contains(package) && package.name.0 == "inventory"));
}

const GAUGES: &str = r#"
import { Measure } from units;
api gauges@v1 {
  title "Gauges";
  description "Gauge readings.";
  rpc Read { input Measure; output Measure; }
  capabilities {
    capability gauges.read { title "Read gauges"; description "Read gauge values."; grants "Sees gauge values."; allows { rpc Read; } }
  }
}
service Gauge { implements gauges; }
"#;

// A package's types are importable once one of its APIs uses them.
const UNITS: &str = r#"
model Measure { value: string; }
api units@v1 {
  title "Units";
  description "Units of measure.";
  rpc Convert { input Measure; output Measure; }
  capabilities {
    capability units.convert { title "Convert"; description "Convert units."; grants "Converts units."; allows { rpc Convert; } }
  }
}
"#;

#[test]
fn a_dependency_nothing_holds_is_reported_by_name_until_its_files_are_supplied() {
    let f = fixture();
    let root = package(
        "[package]\nname = \"gauges\"\nversion = \"1.0.0\"\n\n[sources]\ngauges = \"gauges.trellis\"\n\n\
        [dependencies]\nunits = { package = \"units\", version = \"^2.1.0\", registry = \"ghcr.io/example\" }\n",
        "gauges.trellis",
        GAUGES,
    );
    let units = |version: &str| {
        package(
            &format!("[package]\nname = \"units\"\nversion = \"{version}\"\n\n[sources]\nunits = \"units.trellis\"\n"),
            "units.trellis",
            UNITS,
        )
    };

    let alone = resolve_with(&f, serde_json::json!({ "root": root }));
    assert!(alone.package_evidence.is_none());
    let [missing] = alone.missing.as_slice() else {
        panic!("{:?}", alone.missing)
    };
    assert_eq!(missing.name.0, "units");
    assert_eq!(missing.requirements, ["^2.1.0"]);
    assert_eq!(missing.required_by[0].0, "gauges");

    // A supplied version outside the requirement does not satisfy it.
    let old = resolve_with(
        &f,
        serde_json::json!({ "root": root, "packages": [units("2.0.0")] }),
    );
    assert!(old.package_evidence.is_none());
    assert_eq!(old.missing[0].available_versions[0].0, "2.0.0");

    // A supplied package that doesn't compile is named, not failed as a
    // whole request, so the client can tell which package to fix.
    let broken = resolve_with(
        &f,
        serde_json::json!({ "root": root, "packages": [package(
            "[package]\nname = \"units\"\nversion = \"2.1.3\"\n\n[sources]\nunits = \"units.trellis\"\n",
            "units.trellis",
            "model Measure { value: }",
        )] }),
    );
    assert!(broken.package_evidence.is_none());
    let [rejected] = broken.rejected.as_slice() else {
        panic!("{:?}", broken.rejected)
    };
    assert_eq!(rejected.name.0, "units");

    let supplied = resolve_with(
        &f,
        serde_json::json!({ "root": root, "packages": [units("2.1.3")] }),
    );
    assert!(supplied.missing.is_empty(), "{:?}", supplied.missing);
    assert_eq!(supplied.dependencies[0].origin, AuthPackageOrigin::Supplied);
    let evidence = supplied.package_evidence.unwrap();
    assert_eq!(
        evidence
            .packages
            .iter()
            .map(|package| (package.name.0.as_str(), package.version.0.as_str()))
            .collect::<Vec<_>>(),
        [("gauges", "1.0.0"), ("units", "2.1.3")]
    );
    create(&f, "Gauge", "gauges.Gauge", evidence);
}

#[test]
fn without_a_manifest_imports_name_their_packages_only_when_trellis_holds_them() {
    let f = fixture();
    let files = |source: &str| {
        serde_json::json!({
            "root": {
                "name": "plain",
                "version": "0.1.0",
                "files": [{ "path": "plain.trellis", "text": source }],
            }
        })
    };
    let known = resolve_with(
        &f,
        files("import { health } from trellis;\nservice Plain { use health { capability health.query; } }"),
    );
    assert!(known.unknown_imports.is_empty());
    assert_eq!(known.dependencies[0].origin, AuthPackageOrigin::Builtin);
    assert!(known.package_evidence.is_some());

    let unknown = resolve_with(
        &f,
        files("import { Thing } from elsewhere;\nservice Plain { }"),
    );
    assert_eq!(unknown.unknown_imports, ["elsewhere"]);
    assert!(unknown.package_evidence.is_none());
}
