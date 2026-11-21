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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        .any(|participant| participant.participant_id
            == "localhost/runtime-trellis.LiveV1Provider"
            && participant.kind == AuthDeploymentKind::Service));

    let built = trellis_cli::app::deploy::compile_participant_input(
        &project,
        Some("localhost/runtime-trellis.LiveV1Provider"),
        Some(trellis_idl::ParticipantKind::Service),
    )
    .unwrap();
    let evidence = resolved.package_evidence.unwrap();
    assert_eq!(evidence, built.package_evidence);

    let created = create(
        &f,
        "Live",
        "localhost/runtime-trellis.LiveV1Provider",
        evidence,
    );
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
    format!("[package]\nname = \"inventory\"\nworkspace = \"ghcr.io/example\"\nversion = \"{version}\"\n\n[sources]\nmain = \"inventory.trellis\"\n")
}

fn shop_manifest(requirement: &str) -> String {
    format!(
        "[package]\nname = \"shop\"\nversion = \"1.0.0\"\n\n[sources]\nshop = \"src/shop.trellis\"\n\n\
        [dependencies]\ninventory = {{ package = \"inventory\", workspace = \"ghcr.io/example\", version = \"{requirement}\" }}\n"
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
    let warehouse = create(
        &f,
        "Warehouse",
        "ghcr.io/example/inventory.Warehouse",
        first.clone(),
    );
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
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
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
        (
            "ghcr.io/example/inventory",
            "1.2.0",
            &AuthPackageOrigin::Installed
        )
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
        .find(|package| package.name.0 == "ghcr.io/example/inventory")
        .unwrap();
    assert!(evidence.packages.contains(installed));
    create(&f, "Shop", "localhost/shop.Shop", evidence);

    // The requirement decides, not recency.
    let pinned = shop("=1.0.0");
    assert_eq!(pinned.dependencies[0].version.0, "1.0.0");
    assert!(pinned
        .package_evidence
        .unwrap()
        .packages
        .iter()
        .any(|package| first.packages.contains(package)
            && package.name.0 == "ghcr.io/example/inventory"));
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
        [dependencies]\nunits = { package = \"units\", workspace = \"ghcr.io/example\", version = \"^2.1.0\" }\n",
        "gauges.trellis",
        GAUGES,
    );
    let units_in = |workspace: &str, version: &str| {
        package(
            &format!("[package]\nname = \"units\"\nworkspace = \"{workspace}\"\nversion = \"{version}\"\n\n[sources]\nunits = \"units.trellis\"\n"),
            "units.trellis",
            UNITS,
        )
    };
    let units = |version: &str| units_in("ghcr.io/example", version);

    let alone = resolve_with(&f, serde_json::json!({ "root": root }));
    assert!(alone.package_evidence.is_none());
    let [missing] = alone.missing.as_slice() else {
        panic!("{:?}", alone.missing)
    };
    assert_eq!(missing.name.0, "ghcr.io/example/units");
    assert_eq!(missing.requirements, ["^2.1.0"]);
    assert_eq!(missing.required_by[0].0, "localhost/gauges");

    // A package of the same name from another workspace is another package.
    let elsewhere = resolve_with(
        &f,
        serde_json::json!({ "root": root, "packages": [units_in("ghcr.io/other", "2.1.3")] }),
    );
    assert!(elsewhere.package_evidence.is_none());
    assert_eq!(elsewhere.missing[0].name.0, "ghcr.io/example/units");

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
            "[package]\nname = \"units\"\nworkspace = \"ghcr.io/example\"\nversion = \"2.1.3\"\n\n[sources]\nunits = \"units.trellis\"\n",
            "units.trellis",
            "model Measure { value: }",
        )] }),
    );
    assert!(broken.package_evidence.is_none());
    let [rejected] = broken.rejected.as_slice() else {
        panic!("{:?}", broken.rejected)
    };
    assert_eq!(rejected.name.0, "ghcr.io/example/units");
    assert_eq!(rejected.index, 0);

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
        [
            ("ghcr.io/example/units", "2.1.3"),
            ("localhost/gauges", "1.0.0")
        ]
    );
    create(&f, "Gauge", "localhost/gauges.Gauge", evidence);
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

const SAMPLES: &str = r#"
model Sample { id: string; }
api samples@v1 {
  title "Samples";
  description "Lab samples.";
  rpc Get { input Sample; output Sample; }
  capabilities {
    capability samples.read { title "Read samples"; description "Read samples."; grants "Sees samples."; allows { rpc Get; } }
  }
}
"#;

#[test]
fn same_named_packages_from_two_workspaces_install_with_their_own_api_identities() {
    let f = fixture();
    let theirs_manifest = "[package]\nname = \"krishi\"\nworkspace = \"ghcr.io/qlever-llc\"\n\
        version = \"1.0.0\"\n\n[sources]\nsamples = \"samples.trellis\"\n";
    let theirs_source = format!(
        "{}service Lab {{ implements samples; }}\n",
        SAMPLES.replace("Sample", "Record")
    );
    let theirs = resolve_with(
        &f,
        serde_json::json!({ "root": package(theirs_manifest, "samples.trellis", &theirs_source) }),
    );
    create(
        &f,
        "QLever lab",
        "ghcr.io/qlever-llc/krishi.Lab",
        theirs.package_evidence.unwrap(),
    );

    // Krishi's own `krishi` depends on the other one under the alias qlever.
    let ours = resolve_with(
        &f,
        serde_json::json!({
            "root": package(
                "[package]\nname = \"krishi\"\nworkspace = \"ghcr.io/krishi-diagnostics\"\n\
                version = \"2.1.0\"\n\n[sources]\nsherpa = \"sherpa.trellis\"\n\n\
                [dependencies]\nqlever = { package = \"krishi\", workspace = \
                \"ghcr.io/qlever-llc\", version = \"^1.0\" }\n",
                "sherpa.trellis",
                &format!(
                    "import {{ samples as theirs }} from qlever;\n{SAMPLES}\
                    service SherpaService {{ implements samples; use theirs {{ capability \
                    samples.read; }} }}\n"
                ),
            ),
        }),
    );
    assert!(ours.missing.is_empty(), "{:?}", ours.missing);
    let [qlever] = ours.dependencies.as_slice() else {
        panic!("{:?}", ours.dependencies)
    };
    assert_eq!(
        (qlever.name.0.as_str(), &qlever.origin),
        ("ghcr.io/qlever-llc/krishi", &AuthPackageOrigin::Installed)
    );
    let evidence = ours.package_evidence.unwrap();
    assert_eq!(
        evidence
            .packages
            .iter()
            .map(|package| package.name.0.as_str())
            .collect::<Vec<_>>(),
        [
            "ghcr.io/krishi-diagnostics/krishi",
            "ghcr.io/qlever-llc/krishi"
        ]
    );
    // Its own API is not the other package's, so nothing is shared.
    create(
        &f,
        "Sherpa",
        "ghcr.io/krishi-diagnostics/krishi.SherpaService",
        evidence,
    );
    let accepted = f
        .store
        .transaction(|connection| {
            let mut statement = connection.prepare(
                "SELECT api_id FROM auth_accepted_apis WHERE api_id LIKE 'ghcr.io/%' \
                ORDER BY api_id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    assert_eq!(
        accepted,
        [
            "ghcr.io/krishi-diagnostics/krishi.samples@v1",
            "ghcr.io/qlever-llc/krishi.samples@v1"
        ]
    );
}
