/**
 * Selects the provider protocol build for services and devices.
 *
 * Imported for its effect by the request-serving entrypoints, before anything
 * uses the protocol, so callers never download the provider build.
 *
 * @module
 */
import * as providerProtocolWasm from "./protocol_wasm/provider/trellis_protocol_wasm.js";
import { selectProviderProtocolWasm } from "./protocol_wasm.ts";

selectProviderProtocolWasm(
  providerProtocolWasm,
  new URL(
    "./protocol_wasm/provider/trellis_protocol_wasm_bg.wasm",
    import.meta.url,
  ),
);
