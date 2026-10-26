use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use trellis_idl::project::{GenerateConfig, PackageManifest, PackageMetadata};
use trellis_idl::SourceUnit;

const CONTRACT: &str = r#"
type Count = uint64;
type Signed = int64;
type Blob = bytes;
type Label = string;
type Code = string(min_length=1, max_length=3);
enum Status { ready; }
model Empty {}
model Historic { label: string; }
model Node {
  label: Label;
  parent?: Node;
  children: list<Node>;
  count: Count;
  signed: Signed;
  ratio: number;
  blob: Blob;
  nullable: Node | null;
  status: Status;
  settings: json;
  note?: json;
  code?: Code;
  at?: timestamp;
  ref?: ulid;
}
model Current { root: Node; }

api primary@v1 {
  title "Primary";
  description "Primary fixture API.";
  error Known(Node);
  rpc Fetch { input Node; output Node; errors [Known]; }
  operation Process {
    input Node;
    output Node;
    progress Node;
    errors [Known];
    signals { retry Node; }
    upload;
  }
  event Changed { payload Node; }
  live Watch { input Empty; event Node; }
  capabilities {
    public { allows { publish event Changed; } }
    capability fixture.access {
      title "Access";
      description "Fixture access.";
      grants "The caller can use the fixture.";
      allows { rpc Fetch; operation Process; subscribe event Changed; live Watch; }
    }
  }
}

api secondary@v2 {
  title "Secondary";
  description "Secondary fixture API.";
  event Ping { payload Empty; }
  live Monitor { input Empty; event Empty; }
  capabilities {
    public { allows { publish event Ping; } }
    capability fixture.observe {
      title "Observe";
      description "Fixture observation.";
      grants "The caller can observe the fixture.";
      allows { subscribe event Ping; live Monitor; }
    }
  }
}

service Worker {
  implements primary;
  implements secondary;
  kv optional cache {
    title "Cache";
    description "Migrated cache.";
    schema Current;
    version 2;
    accepts { 1: Historic; }
  }
  store optional blobs { title "Blobs"; description "Optional blobs."; }
}

app Caller {
  use primary {
    capability fixture.access;
  }
  use secondary { required capability fixture.observe; }
}
"#;

fn run(command: &mut Command, label: &str) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_package_exercises_wo03_b2_b4() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap();
    let manifest = PackageManifest {
        package: PackageMetadata {
            name: "fixture".into(),
            version: "3.4.5".parse().unwrap(),
        },
        sources: BTreeMap::from([("fixture".into(), "fixture.trellis".into())]),
        dependencies: BTreeMap::new(),
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    };
    let compile = |source: String| {
        trellis_idl::compile_project(
            &manifest,
            vec![SourceUnit {
                alias: "fixture".into(),
                path: "fixture.trellis".into(),
                source,
            }],
            BTreeMap::new(),
        )
    };
    let graph = compile(CONTRACT.into()).unwrap();

    let temp = tempfile::tempdir_in(repo.join("target")).unwrap();
    let rust = temp.path().join("rust");
    let typescript = temp.path().join("typescript");
    trellis_codegen_rust::generate_rust_package(&graph, &rust, "wo03-fixture", &[]).unwrap();
    trellis_codegen_ts::generate_ts_package(&graph, &typescript, "@fixture/wo03").unwrap();

    let runtime = repo.join("crates/trellis").canonicalize().unwrap();
    let manifest = fs::read_to_string(rust.join("Cargo.toml"))
        .unwrap()
        .replace(
            "trellis-rs = \"0.100.0\"",
            &format!("trellis-rs = {{ path = {runtime:?} }}"),
        );
    // Repository-owned temporary output must not join the enclosing workspace.
    fs::write(
        rust.join("Cargo.toml"),
        format!("{manifest}\n[workspace]\n"),
    )
    .unwrap();
    fs::create_dir(rust.join("tests")).unwrap();
    fs::write(
        rust.join("tests/fixture.rs"),
        r#"
use trellis_rs::generated::{Codec as _, ParticipantDescriptor as _, TrellisError as _};
use wo03_fixture::{
    apis::{fixture_primary_v1, fixture_secondary_v2},
    apps::fixture_caller,
    participants::fixture_worker,
    types::{Node, Number, Status},
};
use fixture_worker::types::{Current, Historic};

fn wire() -> serde_json::Value {
    serde_json::json!({
        "label": "root",
        "parent": {
            "label": "parent",
            "children": [],
            "count": "1",
            "signed": "-1",
            "ratio": 1.0,
            "blob": "AA==",
            "nullable": null,
            "status": "ready",
            "settings": null
        },
        "children": [],
        "count": "18446744073709551615",
        "signed": "-9223372036854775808",
        "ratio": 1.5,
        "blob": "AQID",
        "nullable": null,
        "status": "future",
        "settings": {"a": [1, "x", null], "b": {"c": true}},
        "note": null,
        "ignored": true
    })
}

#[test]
fn codecs_errors_and_generated_surfaces_compile() {
    let node = Node::decode(wire()).unwrap();
    assert_eq!(**node.count, u64::MAX);
    assert_eq!(**node.signed, i64::MIN);
    assert_eq!(&**node.blob, &[1, 2, 3]);
    assert!(matches!(node.status, Status::Unknown(ref value) if value == "future"));
    assert_eq!(node.parent.as_deref().unwrap().label.as_ref(), "parent");
    // `json` carries any value unchecked, including a required or present null.
    assert_eq!(
        node.settings,
        serde_json::json!({"a": [1, "x", null], "b": {"c": true}})
    );
    assert_eq!(node.note, Some(serde_json::Value::Null));
    assert_eq!(node.parent.as_deref().unwrap().settings, serde_json::Value::Null);
    assert_eq!(node.parent.as_deref().unwrap().note, None);
    let encoded = node.encode().unwrap();
    assert_eq!(encoded["settings"], wire()["settings"]);
    assert_eq!(encoded.get("note"), Some(&serde_json::Value::Null));
    assert!(encoded["parent"].get("note").is_none());
    assert_eq!(encoded["count"], "18446744073709551615");
    assert_eq!(encoded["signed"], "-9223372036854775808");
    assert_eq!(encoded["blob"], "AQID");
    assert_eq!(encoded.get("ignored"), Some(&serde_json::json!(true)));
    assert!(Number::from(f64::NAN).encode().is_err());

    type Fetch = fixture_primary_v1::rpc::Fetch;
    let mut known = wire();
    let object = known.as_object_mut().unwrap();
    object.insert("id".into(), "error-1".into());
    object.insert("type".into(), fixture_primary_v1::errors::Known::TYPE.into());
    object.insert("message".into(), "known".into());
    let decoded = <Fetch as trellis_rs::generated::RpcDescriptor>::decode_error(known)
        .unwrap()
        .expect("known error");
    assert!(matches!(decoded, fixture_primary_v1::rpc::FetchError::Known(_)));
    assert!(<Fetch as trellis_rs::generated::RpcDescriptor>::decode_error(
        serde_json::json!({"type":"fixture.primary@v1::Future"})
    )
    .unwrap()
    .is_none());

    type Process = fixture_primary_v1::operations::Process;
    fn signal<S: trellis_rs::generated::OperationSignal<Operation = Process>>() {}
    signal::<fixture_primary_v1::operations::ProcessRetrySignal>();
    fn live<D: trellis_rs::generated::LiveDescriptor>() {}
    fn event<D: trellis_rs::generated::EventDescriptor>() {}
    live::<fixture_primary_v1::lives::Watch>();
    event::<fixture_primary_v1::events::Changed>();
    event::<fixture_secondary_v2::events::Ping>();

    fixture_worker::Participant::validate().unwrap();
    async fn providers(
        provider: &mut fixture_worker::Provider<'_>,
        migrations: &fixture_worker::Migrations,
    ) {
        let _ = provider.fixture_primary_v1();
        let _ = provider.fixture_secondary_v2();
        let _: trellis_rs::service::KvHandle<Current> = provider.cache(migrations).await.unwrap();
        let _: Option<&trellis_rs::service::StoreHandle> = provider.blobs();
    }
    let _ = providers;
    let migrations = fixture_worker::Migrations::new(|Historic { label, .. }| async move {
        Ok::<_, std::convert::Infallible>(Current {
            root: serde_json::from_value(serde_json::json!({
                "label": label,
                "children": [],
                "count": "0",
                "signed": "0",
                "ratio": 0.0,
                "blob": "",
                "nullable": null,
                "status": "ready"
            })).unwrap(),
            extra: Default::default(),
        })
    });
    let _ = migrations;
    async fn caller(client: &fixture_caller::Client, node: &Node) {
        let _: Node = client.fixture_primary_v1().fetch(node).await.unwrap();
    }
    let _ = caller;
}
"#,
    )
    .unwrap();
    // Exercise the current candidate closure, including independently versioned models.
    run(
        Command::new("cargo")
            .arg("test")
            .arg("--quiet")
            .arg("--config")
            .arg(format!(
                "patch.crates-io.trellis-rs.path=\"{}\"",
                repo.join("crates/trellis").display()
            ))
            .arg("--config")
            .arg(format!(
                "patch.crates-io.trellis-protocol.path={:?}",
                repo.join("crates/protocol")
            ))
            .arg("--config")
            .arg(format!(
                "patch.crates-io.trellis-models.path={:?}",
                repo.join("crates/models")
            ))
            .current_dir(&rust),
        "generated Rust test",
    );

    let packages = temp.path().join("node_modules/@oatscenter");
    fs::create_dir_all(&packages).unwrap();
    // Exercise generated code against SDK source, independently of npm staging.
    for name in ["trellis", "result"] {
        std::os::unix::fs::symlink(
            repo.join(format!("ts/packages/{name}")),
            packages.join(name),
        )
        .unwrap();
    }
    fs::write(temp.path().join("package.json"), r#"{"type":"module"}"#).unwrap();
    fs::write(
        temp.path().join("tsconfig.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "compilerOptions": {
                "target": "ES2024",
                "module": "NodeNext",
                "strict": true,
                "noEmit": true,
                "preserveSymlinks": true,
                "allowImportingTsExtensions": true,
                "types": ["node"],
                "typeRoots": [repo.join("node_modules/@types")]
            },
            "include": ["fixture_test.ts"]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        temp.path().join("fixture_test.ts"),
        r#"
import { test } from "node:test";
import { API as Primary, Known } from "./typescript/apis/primary/mod.js";
import { request as Caller } from "./typescript/apps/Caller/mod.js";
import { participant as Worker } from "./typescript/participants/Worker/mod.js";
import type * as WorkerModule from "./typescript/participants/Worker/mod.js";
import type { AppRequestDescriptor } from "@oatscenter/trellis/generated";
import { newUlid, parseTimestamp, parseUlid, timestamp } from "@oatscenter/trellis";

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

const wire = {
  label: "root",
  parent: {
    label: "parent",
    children: [],
    count: "1",
    signed: "-1",
    ratio: 1,
    blob: "AA==",
    nullable: null,
    status: "ready",
    settings: null,
  },
  children: [],
  count: "18446744073709551615",
  signed: "-9223372036854775808",
  ratio: 1.5,
  blob: "AQID",
  nullable: null,
  status: "future",
  settings: { a: [1, "x", null], b: { c: true } },
  note: null,
  ignored: true,
};

test("generated TypeScript package exercises WO-03 B2-B4", () => {
  const fetch = Primary.actions["rpc:Fetch"];
  const node = fetch.input.decode(wire);
  assert(node.count === 18_446_744_073_709_551_615n, "uint64 decode");
  assert(node.signed === -9_223_372_036_854_775_808n, "int64 decode");
  assert(node.blob instanceof Uint8Array && node.blob.join() === "1,2,3", "bytes decode");
  assert(node.nullable === null && node.status === "future", "nullable/open enum decode");
  assert(node.parent?.label === "parent", "recursive model decode");
  assert("ignored" in node, "open model preserves unknown output fields");
  const encoded = fetch.input.encode(node) as Record<string, unknown>;
  assert(encoded.ignored === true, "unknown output fields round-trip");
  assert(encoded.count === wire.count && encoded.signed === wire.signed, "bigint encode");
  assert(encoded.blob === wire.blob, "bytes encode");
  assert(
    JSON.stringify(encoded.settings) === JSON.stringify(wire.settings) &&
      encoded.note === null &&
      !("note" in (encoded.parent as Record<string, unknown>)),
    "json values round-trip exactly, including present nulls",
  );
  let rejectedFunction = false;
  try {
    fetch.input.encode({ ...node, settings: { f: () => 1 } } as never);
  } catch {
    rejectedFunction = true;
  }
  assert(rejectedFunction, "json rejects values JSON cannot represent");
  // Declared limits hold in both directions, counting characters.
  assert(fetch.input.decode({ ...wire, code: "ab\u{1F600}" }).code === "ab\u{1F600}", "within limits");
  for (const code of ["", "abcd"]) {
    let rejected = false;
    try {
      fetch.input.decode({ ...wire, code });
    } catch {
      rejected = true;
    }
    assert(rejected, `decode rejects ${JSON.stringify(code)} outside min_length=1, max_length=3`);
    rejected = false;
    try {
      fetch.input.encode({ ...node, code } as never);
    } catch {
      rejected = true;
    }
    assert(rejected, `encode rejects ${JSON.stringify(code)} outside its limits`);
  }
  // Application code builds timestamps and ULIDs for generated models.
  const stamped: typeof node = {
    ...node,
    at: timestamp(new Date(Date.UTC(2026, 9, 26, 2, 0, 0, 500))),
    ref: newUlid(),
  };
  const stampedWire = fetch.input.encode(stamped) as Record<string, unknown>;
  assert(stampedWire.at === "2026-10-26T02:00:00.5Z", "a Date encodes as canonical UTC");
  assert(fetch.input.decode(stampedWire).ref === stamped.ref, "a new ULID round-trips");
  const parsed = parseTimestamp("2026-10-26T04:00:00+02:00");
  assert(parsed.isOk(), "an offset timestamp parses");
  const fromString: typeof node = { ...node, at: parsed.orThrow() };
  assert(
    (fetch.input.encode(fromString) as Record<string, unknown>).at === "2026-10-26T02:00:00Z",
    "a parsed timestamp is normalized to UTC",
  );
  const late = parseTimestamp("yesterday");
  assert(late.isErr() && late.error.name === "ValidationError", "a malformed timestamp is a ValidationError");
  const existing = parseUlid("01ARZ3NDEKTSV4RRFFQ69G5FAV");
  assert(existing.isOk() && fetch.input.encode({ ...node, ref: existing.orThrow() }) !== undefined, "a parsed ULID fits the model");
  assert(parseUlid("01arz3ndektsv4rrffq69g5fav").isErr(), "a non-canonical ULID is rejected");
  let rejectedNonFinite = false;
  try {
    fetch.input.encode({ ...node, ratio: Number.NaN });
  } catch {
    rejectedNonFinite = true;
  }
  assert(rejectedNonFinite, "non-finite number rejection");

  const knownClass = fetch.errors.find((error) => error.type === Known.type);
  assert(knownClass, "known error descriptor");
  const known = knownClass.fromSerializable({
    ...wire,
    id: "error-1",
    type: Known.type,
    message: "known",
  });
  assert(known.data.count === 18_446_744_073_709_551_615n, "known error payload codec");

  const process = Primary.actions["operation:Process"];
  assert(process.progress?.decode(wire).status === "future", "operation progress");
  assert(process.signals.retry.decode(wire).count === 18_446_744_073_709_551_615n, "operation signal");
  assert(Primary.actions["live:Watch"].event.decode(wire).status === "future", "feed");
  assert(Primary.actions["event:Changed"].payload.decode(wire).status === "future", "event");

  assert(Worker.resources.cache.migrations[1].decode({ label: "old" }).label === "old", "historic codec");

  const handles: WorkerModule.ResourceHandles<{ cache: number; blobs: string }> = {
    cache: undefined,
    blobs: undefined,
  };
  void handles;
  const request: AppRequestDescriptor = Caller;
  void request;
});
"#,
    )
    .unwrap();
    run(
        Command::new("node")
            .arg(repo.join("node_modules/typescript/bin/tsc"))
            .arg("--project")
            .arg(temp.path().join("tsconfig.json")),
        "generated TypeScript check",
    );
    run(
        Command::new("node")
            .arg("--experimental-transform-types")
            .arg("--test")
            .arg(temp.path().join("fixture_test.ts")),
        "generated TypeScript test",
    );
}
