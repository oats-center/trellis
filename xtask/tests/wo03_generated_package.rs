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
    capability access {
      title "Access";
      description "Fixture access.";
      consequence "The caller can use the fixture.";
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
    capability observe {
      title "Observe";
      description "Fixture observation.";
      consequence "The caller can observe the fixture.";
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
    rpc Fetch;
    operation Process;
    subscribe event Changed;
    live Watch;
    optional capability access;
  }
  use secondary { subscribe event Ping; live Monitor; }
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
    let legacy = compile(CONTRACT.replacen("progress Node;", "update Node;", 1)).unwrap_err();
    assert!(format!("{legacy:?}").contains("unsupported operation member 'update'"));
    let graph = compile(CONTRACT.into()).unwrap();

    let temp = tempfile::tempdir().unwrap();
    let rust = temp.path().join("rust");
    let typescript = temp.path().join("typescript");
    trellis_codegen_rust::generate_rust_package(&graph, &rust, "wo03-fixture").unwrap();
    trellis_codegen_ts::generate_ts_package(&graph, &typescript, "@fixture/wo03").unwrap();

    let runtime = repo.join("crates/trellis").canonicalize().unwrap();
    let manifest = fs::read_to_string(rust.join("Cargo.toml"))
        .unwrap()
        .replace(
            "trellis-rs = \"0.100.0\"",
            &format!("trellis-rs = {{ path = {runtime:?} }}"),
        );
    fs::write(rust.join("Cargo.toml"), manifest).unwrap();
    fs::create_dir(rust.join("tests")).unwrap();
    fs::write(
        rust.join("tests/fixture.rs"),
        r#"
use trellis_rs::generated::{Codec as _, OperationDescriptor as _, ParticipantDescriptor as _, TrellisError as _};
use wo03_fixture::{
    apis::{fixture_primary_v1, fixture_secondary_v2},
    participants::{fixture_caller, fixture_worker},
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
            "status": "ready"
        },
        "children": [],
        "count": "18446744073709551615",
        "signed": "-9223372036854775808",
        "ratio": 1.5,
        "blob": "AQID",
        "nullable": null,
        "status": "future",
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
    let encoded = node.encode().unwrap();
    assert_eq!(encoded["count"], "18446744073709551615");
    assert_eq!(encoded["signed"], "-9223372036854775808");
    assert_eq!(encoded["blob"], "AQID");
    assert_eq!(encoded.get("ignored"), Some(&serde_json::json!(true)));
    assert!(Number::from(f64::NAN).encode().is_err());

    type Fetch = fixture_primary_v1::rpc::Fetch;
    assert_eq!(Fetch::ERRORS, ["fixture.primary@v1::Known"]);
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
    assert!(Process::HAS_PROGRESS);
    assert!(Process::UPLOAD);
    assert_eq!(Process::SIGNALS, ["retry"]);
    fn live<D: trellis_rs::generated::LiveDescriptor>() {}
    fn event<D: trellis_rs::generated::EventDescriptor>() {}
    live::<fixture_primary_v1::lives::Watch>();
    event::<fixture_primary_v1::events::Changed>();
    event::<fixture_secondary_v2::events::Ping>();

    assert_eq!(fixture_worker::Participant::IMPLEMENTED_API_IDS.len(), 2);
    assert_eq!(fixture_worker::Participant::PATH, "Worker");
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
    let availability = fixture_worker::Availability::default();
    assert!(!availability.resource_cache);
    assert!(!availability.resource_blobs);
    let caller = fixture_caller::Availability::default();
    assert!(!caller.fixture_primary_v1_access);
    assert!(!caller.action_fixture_primary_v1_rpc_fetch);
    let optional_fetch = trellis_rs::generated::OptionalAction::rpc("fixture.primary@v1", "Fetch");
    assert!(matches!(
        trellis_rs::generated::AvailabilitySnapshot::default()
            .require_action(&[optional_fetch], optional_fetch),
        Err(trellis_rs::client::TrellisClientError::AuthorizationUnavailable(_))
    ));
    fn availability_facade(client: &fixture_caller::Client) {
        let _: fixture_caller::Availability = client.availability();
        let _: futures_util::stream::BoxStream<'static, fixture_caller::Availability> =
            client.watch_availability();
    }
    let _ = availability_facade;
}
"#,
    )
    .unwrap();
    // The generated package pins the workspace version; patch the library to
    // the checkout so the test also passes for unpublished prerelease candidates.
    run(
        Command::new("cargo")
            .arg("test")
            .arg("--quiet")
            .arg("--config")
            .arg(format!(
                "patch.crates-io.trellis-rs.path=\"{}\"",
                repo.join("crates/trellis").display()
            ))
            .current_dir(&rust),
        "generated Rust test",
    );

    let generated_support = repo
        .join("ts/packages/trellis/generated.ts")
        .canonicalize()
        .unwrap();
    let participant_runtime = repo
        .join("ts/packages/trellis/participant_runtime/participant.ts")
        .canonicalize()
        .unwrap();
    let caller_runtime = repo
        .join("ts/packages/trellis/caller.ts")
        .canonicalize()
        .unwrap();
    let connection_runtime = repo
        .join("ts/packages/trellis/connection.ts")
        .canonicalize()
        .unwrap();
    let errors_runtime = repo
        .join("ts/packages/trellis/errors/index.ts")
        .canonicalize()
        .unwrap();
    // A local `imports` map replaces the extended map rather than merging with
    // it, so every workspace package the fixture imports must be listed.
    let result_package = repo
        .join("ts/packages/result/mod.ts")
        .canonicalize()
        .unwrap();
    fs::write(
        temp.path().join("deno.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "extends": repo.join("ts/deno.json"),
            "imports": {
                "@oatscenter/result": format!("file://{}", result_package.display()),
                "@oatscenter/trellis/generated": format!("file://{}", generated_support.display()),
                "@fixture/caller": format!("file://{}", caller_runtime.display()),
                "@fixture/connection": format!("file://{}", connection_runtime.display()),
                "@fixture/errors": format!("file://{}", errors_runtime.display()),
                "@fixture/participant-runtime": format!("file://{}", participant_runtime.display())
            },
            "compilerOptions": { "strict": true }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        temp.path().join("fixture_test.ts"),
        r#"
import { API as Primary, Known } from "./typescript/apis/primary/mod.js";
import { API as Secondary } from "./typescript/apis/secondary/mod.js";
import { participant as Caller } from "./typescript/participants/Caller/mod.js";
import { participant as Worker } from "./typescript/participants/Worker/mod.js";
import type * as CallerModule from "./typescript/participants/Caller/mod.js";
import type * as WorkerModule from "./typescript/participants/Worker/mod.js";
import { AsyncResult, ok, Result } from "@oatscenter/result";
import { createCallerRuntime } from "@fixture/caller";
import { installConnectionAvailability, TrellisConnection } from "@fixture/connection";
import { TransportError } from "@fixture/errors";
import { getParticipantRuntime, participantAvailability } from "@fixture/participant-runtime";

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
  },
  children: [],
  count: "18446744073709551615",
  signed: "-9223372036854775808",
  ratio: 1.5,
  blob: "AQID",
  nullable: null,
  status: "future",
  ignored: true,
};

Deno.test("generated TypeScript package exercises WO-03 B2-B4", () => {
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
  const futureError: string = "fixture.primary@v1::Future";
  assert(fetch.errors.every((error) => error.type !== futureError), "unknown error remains unknown");
  const runtimeErrors = getParticipantRuntime(Caller).usedApi.rpc.Fetch.runtimeErrors;
  assert(runtimeErrors?.[0].type === Known.type, "runtime keeps qualified known error type");

  const process = Primary.actions["operation:Process"];
  assert(process.progress?.decode(wire).status === "future", "operation progress");
  assert(process.signals.retry.decode(wire).count === 18_446_744_073_709_551_615n, "operation signal");
  assert(process.upload, "operation upload");
  assert(Primary.actions["live:Watch"].event.decode(wire).status === "future", "feed");
  assert(Primary.actions["event:Changed"].payload.decode(wire).status === "future", "event");
  assert(Secondary.actions["event:Ping"], "second API");
  assert(Secondary.actions["live:Monitor"], "second API provider action");

  assert(Worker.implements.length === 2, "multi-API participant");
  assert(Worker.path === "Worker" && Caller.path === "Caller", "lexical participant paths");
  assert(Worker.resources.cache.availability === "optional", "optional KV");
  assert(Worker.resources.blobs.availability === "optional", "optional store");
  assert(Worker.resources.cache.migrations[1].decode({ label: "old" }).label === "old", "historic codec");
  assert(Caller.uses[0].optionalCapabilities[0] === "fixture.primary@v1::access", "availability evidence");

  const handles: WorkerModule.ResourceHandles<{ cache: number; blobs: string }> = {
    cache: undefined,
    blobs: undefined,
  };
  assert(handles.cache === undefined && handles.blobs === undefined, "optional handles are explicit");
});

Deno.test("generated caller replaces and enforces installed availability", async () => {
  const connection = new TrellisConnection({
    kind: "client",
    availability: participantAvailability(Caller, {}, {}),
  });
  let transportCalls = 0;
  const caller = createCallerRuntime({
    connection,
    state: {},
    request: () => {
      transportCalls += 1;
      return AsyncResult.from(Promise.resolve(ok(wire)));
    },
    operationHandle: () => ({ input: () => ({}), resume: () => ({}) }),
    feedHandle: () => ({ input: () => ({ subscribe: () => AsyncResult.from(Promise.resolve(ok([]))) }) }),
    publish: () => AsyncResult.from(Promise.resolve(ok(undefined))),
    prepare: () => ok({}),
    listenEvent: () => AsyncResult.from(Promise.resolve(ok(undefined))),
    publishPrepared: () => AsyncResult.from(Promise.resolve(ok(undefined))),
    transfer: () => ({}),
    wait: () => AsyncResult.from(Promise.resolve(ok(undefined))),
  }, Caller);

  const unavailable = await caller.fetch(Primary.actions["rpc:Fetch"].input.decode(wire)).take();
  assert(Result.isErr(unavailable), "unavailable optional action fails");
  assert(unavailable.error instanceof TransportError, "typed unavailable error");
  assert(unavailable.error.code === "trellis.request.unavailable", "typed unavailable error");
  assert(transportCalls === 0, "unavailable action does not reach transport");

  const watcher = caller.watchAvailability()[Symbol.asyncIterator]();
  const initial = await watcher.next();
  const typedInitial: CallerModule.Availability = initial.value;
  assert(!typedInitial.capabilities["fixture.primary@v1::access"], "initial snapshot");
  const replacement = participantAvailability(Caller, { "fixture.primary@v1": {} }, {});
  installConnectionAvailability(connection, replacement);
  const changed = await watcher.next();
  assert(changed.value === caller.availability(), "watch receives installed replacement");
  assert(Object.isFrozen(changed.value) && Object.isFrozen(changed.value.capabilities), "snapshot is immutable");

  const available = await caller.fetch(Primary.actions["rpc:Fetch"].input.decode(wire)).take();
  assert(!Result.isErr(available), "available optional action invokes");
  assert(Number(transportCalls) === 1, "available action reaches transport once");
  await watcher.return?.();
});
"#,
    )
    .unwrap();
    run(
        Command::new("deno")
            .arg("check")
            .arg("--no-lock")
            .arg("-c")
            .arg(temp.path().join("deno.json"))
            .arg(temp.path().join("fixture_test.ts")),
        "generated TypeScript check",
    );
    run(
        Command::new("deno")
            .arg("test")
            .arg("--no-lock")
            .arg("-c")
            .arg(temp.path().join("deno.json"))
            .arg(temp.path().join("fixture_test.ts")),
        "generated TypeScript test",
    );
}
