import { copy } from "@std/fs";

import { buildTypeScriptPackage } from "../../../tools/package_build/build_typescript_package.ts";
import { runtimeDependencies } from "../../../tools/package_build/runtime_dependencies.ts";
import resultConfig from "../../result/deno.json" with { type: "json" };
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
    ...runtimeDependencies([
      "@opentelemetry/api",
      "@opentelemetry/context-async-hooks",
      "@opentelemetry/core",
      "@opentelemetry/exporter-metrics-otlp-proto",
      "@opentelemetry/exporter-trace-otlp-proto",
      "@opentelemetry/resources",
      "@opentelemetry/sdk-metrics",
      "@opentelemetry/sdk-trace-base",
      "@opentelemetry/sdk-trace-node",
      "@opentelemetry/sdk-trace-web",
      "@opentelemetry/semantic-conventions",
      "@nats-io/jetstream",
      "@nats-io/kv",
      "@nats-io/obj",
      "@nats-io/nats-core",
      "@nats-io/nkeys",
      "@nats-io/transport-node",
      "@noble/hashes/hkdf",
      "pino",
      "tweetnacl",
      "typebox",
      "ulid",
    ]),
    [resultConfig.name]: `^${resultConfig.version}`,
  },
}, config.version);

await copy("internal_sdk/generated", "npm/internal_sdk/generated");

await Deno.mkdir("npm/auth/protocol_wasm", { recursive: true });
await Deno.mkdir("npm/auth/authorization", { recursive: true });
await Deno.copyFile(
  "auth/authorization/verification_worker.mjs",
  "npm/auth/authorization/verification_worker.mjs",
);
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
