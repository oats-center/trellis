import assert from "node:assert/strict";
import { test } from "node:test";
import {
  AuthError,
  getBuiltinRpcError,
  OperationAlreadyTerminalError,
  OperationMismatchError,
  OperationNotFoundError,
  RemoteError,
  TransportError,
  ValidationError,
} from "../errors/index.ts";
import { Result, UnexpectedError } from "../../result/mod.ts";

test("Verify errors serialize and validate", async (t) => {
  await t.test("UnexpectedError", () => {
    const error = new UnexpectedError({ context: { userId: "123" } });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "UnexpectedError");
    assert.deepEqual(value.message, "An unexpected error has occurred");
    assert.deepEqual(value.context, { userId: "123" });
  });

  await t.test("AuthError", () => {
    const error = new AuthError({ reason: "invalid_request" });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "AuthError");
    assert.deepEqual(value.message, "Auth failed: invalid_request");
    assert.deepEqual(Reflect.get(value, "reason"), "invalid_request");
  });

  await t.test("AuthError supports identity_not_found", () => {
    const error = new AuthError({ reason: "identity_not_found" });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "AuthError");
    assert.deepEqual(Reflect.get(value, "reason"), "identity_not_found");
  });

  await t.test("AuthError supports user_already_exists", () => {
    const error = new AuthError({ reason: "user_already_exists" });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "AuthError");
    assert.deepEqual(Reflect.get(value, "reason"), "user_already_exists");
  });

  await t.test("TransportError", () => {
    const error = new TransportError({
      code: "trellis.transport.unavailable",
      message: "Trellis could not reach the requested capability.",
      hint: "Check that the target service is installed and reachable, then try again.",
      context: { subject: "rpc.v1.Example.Run" },
      traceId: "trace-123",
    });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "TransportError");
    assert.deepEqual(
      Reflect.get(value, "code"),
      "trellis.transport.unavailable",
    );
    assert.deepEqual(
      Reflect.get(value, "hint"),
      "Check that the target service is installed and reachable, then try again.",
    );
    assert.deepEqual(value.context, { subject: "rpc.v1.Example.Run" });
    assert.deepEqual(value.traceId, "trace-123");

    const runtimeError = getBuiltinRpcError("TransportError");
    assert(runtimeError, "Expected builtin transport error descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof TransportError);
    assert.deepEqual(reconstructed.message, error.message);
    assert.deepEqual(reconstructed.code, "trellis.transport.unavailable");
    assert.deepEqual(
      reconstructed.hint,
      "Check that the target service is installed and reachable, then try again.",
    );
    assert.deepEqual(reconstructed.getContext(), {
      subject: "rpc.v1.Example.Run",
    });
    assert.deepEqual(reconstructed.toSerializable().traceId, "trace-123");
  });

  await t.test("OperationNotFoundError", () => {
    const error = new OperationNotFoundError({ operationId: "op-missing" });
    const result = RemoteError.parseJSON(error.toJSON());
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "OperationNotFoundError");
    assert.deepEqual(Reflect.get(value, "operationId"), "op-missing");

    const runtimeError = getBuiltinRpcError("OperationNotFoundError");
    assert(runtimeError, "Expected builtin operation not found descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof OperationNotFoundError);
    assert.deepEqual(reconstructed.operationId, "op-missing");
  });

  await t.test("OperationAlreadyTerminalError", () => {
    const error = new OperationAlreadyTerminalError({
      operationId: "op-terminal",
      state: "completed",
      operation: "refund",
      service: "billing",
      traceId: "trace-terminal",
    });
    const result = RemoteError.parseJSON(error.toJSON());
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "OperationAlreadyTerminalError");
    assert.deepEqual(Reflect.get(value, "operationId"), "op-terminal");
    assert.deepEqual(Reflect.get(value, "state"), "completed");
    assert.deepEqual(Reflect.get(value, "operation"), "refund");
    assert.deepEqual(Reflect.get(value, "service"), "billing");
    assert.deepEqual(value.traceId, "trace-terminal");

    const runtimeError = getBuiltinRpcError("OperationAlreadyTerminalError");
    assert(runtimeError, "Expected builtin operation terminal descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof OperationAlreadyTerminalError);
    assert.deepEqual(reconstructed.operationId, "op-terminal");
    assert.deepEqual(reconstructed.state, "completed");
    assert.deepEqual(reconstructed.operation, "refund");
    assert.deepEqual(reconstructed.service, "billing");
  });

  await t.test("OperationMismatchError", () => {
    const error = new OperationMismatchError({
      operationId: "op-mismatch",
      expectedService: "billing",
      expectedOperation: "refund",
      actualService: "orders",
      actualOperation: "capture",
    });
    const result = RemoteError.parseJSON(error.toJSON());
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "OperationMismatchError");
    assert.deepEqual(Reflect.get(value, "operationId"), "op-mismatch");
    assert.deepEqual(Reflect.get(value, "expectedService"), "billing");
    assert.deepEqual(Reflect.get(value, "expectedOperation"), "refund");
    assert.deepEqual(Reflect.get(value, "actualService"), "orders");
    assert.deepEqual(Reflect.get(value, "actualOperation"), "capture");

    const runtimeError = getBuiltinRpcError("OperationMismatchError");
    assert(runtimeError, "Expected builtin operation mismatch descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof OperationMismatchError);
    assert.deepEqual(reconstructed.operationId, "op-mismatch");
    assert.deepEqual(reconstructed.expectedService, "billing");
    assert.deepEqual(reconstructed.expectedOperation, "refund");
    assert.deepEqual(reconstructed.actualService, "orders");
    assert.deepEqual(reconstructed.actualOperation, "capture");
  });

  await t.test("ValidationError", () => {
    const error = new ValidationError({
      errors: [
        { path: "/email", message: "Invalid email format" },
        { path: "/age", message: "Must be a number" },
      ],
    });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "ValidationError");
    const issues = Reflect.get(value, "issues");
    assert(Array.isArray(issues));
    assert.deepEqual(issues.length, 2);
    assert.deepEqual(Reflect.get(issues[0], "path"), "/email");
    assert.deepEqual(Reflect.get(issues[0], "message"), "Invalid email format");
  });

  await t.test("returns error on invalid JSON", () => {
    const result = RemoteError.parseJSON("{ invalid json }");
    assert(result.isErr(), "Expected parse to fail");
    const value = result.take();
    assert(Result.isErr(value));
    assert.deepEqual(value.error.name, "UnexpectedError");
  });

  await t.test("accepts unknown remote error types with base fields", () => {
    const invalidData = JSON.stringify({
      id: "123",
      type: "UnknownErrorType",
      message: "Some message",
    });

    const result = RemoteError.parseJSON(invalidData);
    assert(result.isOk(), "Expected parse to succeed");
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");
    assert.deepEqual(value.type, "UnknownErrorType");
  });

  await t.test("returns error on missing required fields", () => {
    const invalidData = JSON.stringify({
      type: "UnexpectedError",
    });

    const result = RemoteError.parseJSON(invalidData);
    assert(result.isErr(), "Expected parse to fail");
    const value = result.take();
    assert(Result.isErr(value));
    assert.deepEqual(value.error.name, "ValidationError");
  });
});
