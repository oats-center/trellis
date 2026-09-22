import { assert, assertEquals } from "@std/assert";
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

Deno.test("Verify errors serialize and validate", async (t) => {
  await t.step("UnexpectedError", () => {
    const error = new UnexpectedError({ context: { userId: "123" } });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "UnexpectedError");
    assertEquals(value.message, "An unexpected error has occurred");
    assertEquals(value.context, { userId: "123" });
  });

  await t.step("AuthError", () => {
    const error = new AuthError({ reason: "invalid_request" });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "AuthError");
    assertEquals(value.message, "Auth failed: invalid_request");
    assertEquals(Reflect.get(value, "reason"), "invalid_request");
  });

  await t.step("AuthError supports identity_not_found", () => {
    const error = new AuthError({ reason: "identity_not_found" });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "AuthError");
    assertEquals(Reflect.get(value, "reason"), "identity_not_found");
  });

  await t.step("AuthError supports user_already_exists", () => {
    const error = new AuthError({ reason: "user_already_exists" });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "AuthError");
    assertEquals(Reflect.get(value, "reason"), "user_already_exists");
  });

  await t.step("TransportError", () => {
    const error = new TransportError({
      code: "trellis.transport.unavailable",
      message: "Trellis could not reach the requested capability.",
      hint:
        "Check that the target service is installed and reachable, then try again.",
      context: { subject: "rpc.v1.Example.Run" },
      traceId: "trace-123",
    });
    const json = error.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "TransportError");
    assertEquals(Reflect.get(value, "code"), "trellis.transport.unavailable");
    assertEquals(
      Reflect.get(value, "hint"),
      "Check that the target service is installed and reachable, then try again.",
    );
    assertEquals(value.context, { subject: "rpc.v1.Example.Run" });
    assertEquals(value.traceId, "trace-123");

    const runtimeError = getBuiltinRpcError("TransportError");
    assert(runtimeError, "Expected builtin transport error descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof TransportError);
    assertEquals(reconstructed.message, error.message);
    assertEquals(reconstructed.code, "trellis.transport.unavailable");
    assertEquals(
      reconstructed.hint,
      "Check that the target service is installed and reachable, then try again.",
    );
    assertEquals(reconstructed.getContext(), { subject: "rpc.v1.Example.Run" });
    assertEquals(reconstructed.toSerializable().traceId, "trace-123");
  });

  await t.step("OperationNotFoundError", () => {
    const error = new OperationNotFoundError({ operationId: "op-missing" });
    const result = RemoteError.parseJSON(error.toJSON());
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "OperationNotFoundError");
    assertEquals(Reflect.get(value, "operationId"), "op-missing");

    const runtimeError = getBuiltinRpcError("OperationNotFoundError");
    assert(runtimeError, "Expected builtin operation not found descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof OperationNotFoundError);
    assertEquals(reconstructed.operationId, "op-missing");
  });

  await t.step("OperationAlreadyTerminalError", () => {
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

    assertEquals(value.type, "OperationAlreadyTerminalError");
    assertEquals(Reflect.get(value, "operationId"), "op-terminal");
    assertEquals(Reflect.get(value, "state"), "completed");
    assertEquals(Reflect.get(value, "operation"), "refund");
    assertEquals(Reflect.get(value, "service"), "billing");
    assertEquals(value.traceId, "trace-terminal");

    const runtimeError = getBuiltinRpcError("OperationAlreadyTerminalError");
    assert(runtimeError, "Expected builtin operation terminal descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof OperationAlreadyTerminalError);
    assertEquals(reconstructed.operationId, "op-terminal");
    assertEquals(reconstructed.state, "completed");
    assertEquals(reconstructed.operation, "refund");
    assertEquals(reconstructed.service, "billing");
  });

  await t.step("OperationMismatchError", () => {
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

    assertEquals(value.type, "OperationMismatchError");
    assertEquals(Reflect.get(value, "operationId"), "op-mismatch");
    assertEquals(Reflect.get(value, "expectedService"), "billing");
    assertEquals(Reflect.get(value, "expectedOperation"), "refund");
    assertEquals(Reflect.get(value, "actualService"), "orders");
    assertEquals(Reflect.get(value, "actualOperation"), "capture");

    const runtimeError = getBuiltinRpcError("OperationMismatchError");
    assert(runtimeError, "Expected builtin operation mismatch descriptor");
    const reconstructed = runtimeError.fromSerializable(value);
    assert(reconstructed instanceof OperationMismatchError);
    assertEquals(reconstructed.operationId, "op-mismatch");
    assertEquals(reconstructed.expectedService, "billing");
    assertEquals(reconstructed.expectedOperation, "refund");
    assertEquals(reconstructed.actualService, "orders");
    assertEquals(reconstructed.actualOperation, "capture");
  });

  await t.step("ValidationError", () => {
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

    assertEquals(value.type, "ValidationError");
    const issues = Reflect.get(value, "issues");
    assert(Array.isArray(issues));
    assertEquals(issues.length, 2);
    assertEquals(Reflect.get(issues[0], "path"), "/email");
    assertEquals(Reflect.get(issues[0], "message"), "Invalid email format");
  });

  await t.step("returns error on invalid JSON", () => {
    const result = RemoteError.parseJSON("{ invalid json }");
    assert(result.isErr(), "Expected parse to fail");
    const value = result.take();
    assert(Result.isErr(value));
    assertEquals(value.error.name, "UnexpectedError");
  });

  await t.step("accepts unknown remote error types with base fields", () => {
    const invalidData = JSON.stringify({
      id: "123",
      type: "UnknownErrorType",
      message: "Some message",
    });

    const result = RemoteError.parseJSON(invalidData);
    assert(result.isOk(), "Expected parse to succeed");
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");
    assertEquals(value.type, "UnknownErrorType");
  });

  await t.step("returns error on missing required fields", () => {
    const invalidData = JSON.stringify({
      type: "UnexpectedError",
    });

    const result = RemoteError.parseJSON(invalidData);
    assert(result.isErr(), "Expected parse to fail");
    const value = result.take();
    assert(Result.isErr(value));
    assertEquals(value.error.name, "ValidationError");
  });
});

Deno.test("Type narrowing", async (t) => {
  await t.step("enables type narrowing based on type field", () => {
    const error = new AuthError({ reason: "forbidden" });
    const json = error.toJSON();
    const result = RemoteError.parseJSON(json);
    const value = result.take();

    assert(!Result.isErr(value), "Expected successful parse");

    if (value.type === "ValidationError") {
      assert(false, "Should not reach this branch");
    } else if (value.type === "UnexpectedError") {
      assert(false, "Should not reach this branch");
    } else if (value.type === "TransportError") {
      assert(false, "Should not reach this branch");
    } else if (value.type === "KVError") {
      assert(false, "Should not reach this branch");
    }
    assertEquals(Reflect.get(value, "reason"), "forbidden");
  });
});

Deno.test("RemoteError - Wrapper pattern", async (t) => {
  await t.step("wraps remote AuthError", () => {
    const remoteError = new AuthError({
      reason: "invalid_request",
      context: { requestId: "req-123" },
    });
    const json = remoteError.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const wrappedError = new RemoteError({ error: value });

    assertEquals(wrappedError.name, "RemoteError");
    assertEquals(
      wrappedError.message,
      "Remote error: Auth failed: invalid_request",
    );
    assertEquals(wrappedError.remoteError.type, "AuthError");

    assertEquals(
      Reflect.get(wrappedError.remoteError, "reason"),
      "invalid_request",
    );
  });

  await t.step("wraps remote ValidationError", () => {
    const remoteError = new ValidationError({
      errors: [
        { path: "/phoneNumber", message: "Required field" },
      ],
    });
    const json = remoteError.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const wrappedError = new RemoteError({ error: value });

    const issues = Reflect.get(wrappedError.remoteError, "issues");
    assert(Array.isArray(issues));
    assertEquals(issues.length, 1);
    assertEquals(Reflect.get(issues[0], "path"), "/phoneNumber");
  });

  await t.step("can add local context to wrapper", () => {
    const remoteError = new UnexpectedError();
    const json = remoteError.toJSON();
    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const wrappedError = new RemoteError({
      error: value,
      context: { serviceUrl: "https://api.example.com" },
    });

    const serialized = wrappedError.toSerializable();
    assertEquals(serialized.context, { serviceUrl: "https://api.example.com" });
  });

  await t.step("serializes with embedded remote error", () => {
    const remoteError = new AuthError({ reason: "forbidden" });
    const json = remoteError.toJSON();
    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const wrappedError = new RemoteError({ error: value });
    const serialized = wrappedError.toSerializable();

    assertEquals(serialized.type, "RemoteError");
    assertEquals(serialized.remoteError.type, "AuthError");

    if (serialized.remoteError.type === "AuthError") {
      assertEquals(serialized.remoteError.reason, "forbidden");
    }
  });

  await t.step("Full remote error handling flow", () => {
    const originalError = new ValidationError({
      errors: [{ path: "/name", message: "Required field" }],
    });

    const json = originalError.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const wrappedError = new RemoteError({
      error: value,
      context: { receivedFrom: "service-a" },
    });

    try {
      throw wrappedError;
    } catch (e) {
      assert(e instanceof RemoteError);

      const issues = Reflect.get(e.remoteError, "issues");
      assert(Array.isArray(issues));
      assertEquals(Reflect.get(issues[0], "message"), "Required field");

      const logData = e.toSerializable();
      assertEquals(logData.type, "RemoteError");
      assertEquals(logData.remoteError.type, "ValidationError");
    }
  });
});
