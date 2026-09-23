import { copy } from "@std/fs";

import { buildTypeScriptPackage } from "../../../tools/package_build/build_typescript_package.ts";
import config from "../deno.json" with { type: "json" };

await buildTypeScriptPackage({
  name: config.name,
  description:
    "Client-side Trellis runtime, models, and participant helpers for TypeScript applications.",
  exports: {
    ".": {
      types: "./index.d.ts",
      import: "./index.js",
    },
    "./generated": { types: "./generated.d.ts", import: "./generated.js" },
    "./auth": { types: "./auth.d.ts", import: "./auth.js" },
    "./auth/browser": {
      types: "./auth/browser.d.ts",
      import: "./auth/browser.js",
    },
    "./device": { types: "./device.d.ts", import: "./device.js" },
    "./errors": { types: "./errors/index.d.ts", import: "./errors/index.js" },
    "./service": { types: "./service/mod.d.ts", import: "./service/mod.js" },
    "./telemetry": { types: "./telemetry.d.ts", import: "./telemetry.js" },
    "./telemetry/browser": {
      types: "./telemetry/browser.d.ts",
      import: "./telemetry/browser.js",
    },
  },
  dependencies: {
    "@opentelemetry/api": "^1.9.1",
    "@opentelemetry/context-async-hooks": "^2.7.0",
    "@opentelemetry/core": "^2.7.0",
    "@opentelemetry/exporter-metrics-otlp-proto": "^0.215.0",
    "@opentelemetry/exporter-trace-otlp-proto": "^0.215.0",
    "@opentelemetry/resources": "^2.7.0",
    "@opentelemetry/sdk-metrics": "^2.7.0",
    "@opentelemetry/sdk-trace-base": "^2.7.0",
    "@opentelemetry/sdk-trace-node": "^2.7.0",
    "@opentelemetry/sdk-trace-web": "^2.7.0",
    "@opentelemetry/semantic-conventions": "^1.40.0",
    "@nats-io/jetstream": "^3.3.1",
    "@nats-io/kv": "^3.3.1",
    "@nats-io/obj": "^3.3.1",
    "@nats-io/nats-core": "^3.3.1",
    "@nats-io/nkeys": "^2.0.3",
    "@nats-io/transport-node": "^3.3.1",
    "@noble/curves": "^2.0.1",
    "@noble/hashes": "1.8.0",
    "@oatscenter/result": "^0.100.0",
    "js-sha256": "^0.11.1",
    pino: "^10.3.1",
    tweetnacl: "^1.0.3",
    "ts-deepmerge": "^7.0.3",
    typebox: "^1.1.33",
    ulid: "^3.0.2",
  },
}, config.version);

await copy("internal_sdk/generated", "npm/internal_sdk/generated");

await Deno.mkdir("npm/auth/protocol_wasm", { recursive: true });
for (
  const file of [
    "trellis_protocol_wasm.js",
    "trellis_protocol_wasm.d.ts",
    "trellis_protocol_wasm_bg.wasm",
  ]
) {
  await Deno.copyFile(
    `auth/protocol_wasm/${file}`,
    `npm/auth/protocol_wasm/${file}`,
  );
}
