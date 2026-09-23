/**
 * Tests for browser-safe exports.
 */

import { assertEquals, assertExists } from "@std/assert";
import { AsyncResult, isErr, isOk, Result } from "@oatscenter/result";

// Import everything from browser.ts to verify exports exist
import {
  // Error types
  AuthError,
  FileInfoSchema,
  KVError,
  type KvWatchItem,
  RemoteError,
  type ResourceRevision,
  StoreError,
  TransferError,
  type TrellisAuth,
  type TrellisErrorInstance,
  type TypedKvEntry,
  TypedStoreEntry,
  UnexpectedError,
  ValidationError,
} from "./index.ts";
import * as browser from "./index.ts";

const browserCspUnsafeSourcePattern = /\bnew\s+Function\b|\beval\s*\(/;

async function assertSourceFileCspSafe(path: string): Promise<void> {
  const source = await Deno.readTextFile(new URL(path, import.meta.url));
  assertEquals(browserCspUnsafeSourcePattern.test(source), false, path);
}

Deno.test("browser source avoids CSP-unsafe evaluation", async () => {
  await Promise.all([
    assertSourceFileCspSafe("./index.ts"),
    assertSourceFileCspSafe("./client_connect.ts"),
    assertSourceFileCspSafe("./runtime_transport.ts"),
    assertSourceFileCspSafe("./telemetry/env.ts"),
    assertSourceFileCspSafe("./telemetry/runtime.ts"),
  ]);
});

Deno.test("browser exports exclude raw runtime constructors", () => {
  assertEquals("Trellis" in browser, false);
  assertEquals("TypedKV" in browser, false);
  assertEquals("TypedStore" in browser, false);
  assertEquals("createTransferHandle" in browser, false);
});

Deno.test("browser exports - TypedStoreEntry class is exported", () => {
  assertExists(TypedStoreEntry, "TypedStoreEntry class should be exported");
  assertEquals(
    typeof TypedStoreEntry,
    "function",
    "TypedStoreEntry should be a constructor",
  );
});

Deno.test("browser exports - Result utilities are exported", () => {
  assertExists(Result, "Result class should be exported");
  assertExists(isOk, "isOk function should be exported");
  assertExists(isErr, "isErr function should be exported");
  assertExists(AsyncResult, "AsyncResult class should be exported");

  assertEquals(typeof isOk, "function", "isOk should be a function");
  assertEquals(typeof isErr, "function", "isErr should be a function");
});

Deno.test("browser exports - Result utilities work correctly", () => {
  const okResult = Result.ok(42);
  const errResult = Result.err(new UnexpectedError({}));

  // isOk/isErr work on Result instances
  assertEquals(isOk(okResult), true, "isOk should return true for ok results");
  assertEquals(
    isErr(errResult),
    true,
    "isErr should return true for err results",
  );

  // isErr also works on take() output for early return pattern
  const errValue = errResult.take();
  assertEquals(isErr(errValue), true, "isErr should work on take() output");

  const okValue = okResult.take();
  assertEquals(
    isErr(okValue),
    false,
    "isErr should return false for ok take() values",
  );
});

Deno.test("browser exports - Error types are exported", () => {
  assertExists(AuthError, "AuthError should be exported");
  assertExists(ValidationError, "ValidationError should be exported");
  assertExists(RemoteError, "RemoteError should be exported");
  assertExists(KVError, "KVError should be exported");
  assertExists(StoreError, "StoreError should be exported");
  assertExists(TransferError, "TransferError should be exported");
  assertExists(UnexpectedError, "UnexpectedError should be exported");

  assertEquals(
    typeof AuthError,
    "function",
    "AuthError should be a constructor",
  );
  assertEquals(
    typeof ValidationError,
    "function",
    "ValidationError should be a constructor",
  );
  assertEquals(
    typeof RemoteError,
    "function",
    "RemoteError should be a constructor",
  );
  assertEquals(typeof KVError, "function", "KVError should be a constructor");
  assertEquals(
    typeof StoreError,
    "function",
    "StoreError should be a constructor",
  );
  assertEquals(
    typeof TransferError,
    "function",
    "TransferError should be a constructor",
  );
  assertEquals(
    typeof UnexpectedError,
    "function",
    "UnexpectedError should be a constructor",
  );

  type _TestTrellisErrorInstance = TrellisErrorInstance;
});

Deno.test("browser exports - Error types can be instantiated", () => {
  const authErr = new AuthError({ reason: "invalid_request" });
  const validationErr = new ValidationError({
    errors: [{ path: "field", message: "required" }],
  });
  const kvErr = new KVError({ operation: "get" });
  const storeErr = new StoreError({ operation: "get" });
  const transferErr = new TransferError({ operation: "put" });
  const unexpectedErr = new UnexpectedError({});

  assertExists(authErr, "AuthError should be instantiable");
  assertExists(validationErr, "ValidationError should be instantiable");
  assertExists(kvErr, "KVError should be instantiable");
  assertExists(storeErr, "StoreError should be instantiable");
  assertExists(transferErr, "TransferError should be instantiable");
  assertExists(unexpectedErr, "UnexpectedError should be instantiable");
});

Deno.test("browser exports - file schemas are exported", () => {
  assertExists(FileInfoSchema, "FileInfoSchema should be exported");
});

// Type-level tests (these compile if types are correctly exported)
Deno.test("browser exports - types compile correctly", () => {
  // These type annotations verify the types are exported
  const _auth: TrellisAuth = {
    sessionKey: "test",
    sign: async (_data: Uint8Array) => new Uint8Array(),
  };

  const _entry: TypedKvEntry<string> = {
    operation: "put",
    key: "test",
    value: "value",
    revision: 1 as ResourceRevision,
    timestamp: new Date(),
  };
  const _watchItem: KvWatchItem<string> = Result.ok(_entry);

  assertEquals(true, true, "Types should compile without errors");
});
