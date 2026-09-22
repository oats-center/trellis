import { assert, assertEquals } from "@std/assert";
import {
  AuthError,
  machineErrorCode,
  OperationAlreadyTerminalError,
  OperationMismatchError,
  OperationNotFoundError,
  RemoteError,
  ValidationError,
} from "../errors/index.ts";
import { Result } from "../../result/mod.ts";

Deno.test("AuthError", async (t) => {
  await t.step("serialization and validation", () => {
    const original = new AuthError({
      reason: "invalid_request",
      context: { userId: "123", endpoint: "/api/users" },
    });

    const json = original.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "AuthError");
    if (value.type === "AuthError" && "reason" in value) {
      assertEquals(value.id, original.id);
      assertEquals(value.reason, "invalid_request");
      assertEquals(value.context, { userId: "123", endpoint: "/api/users" });
    }
  });

  await t.step("serialization includes all fields", () => {
    const error = new AuthError({
      reason: "forbidden",
      context: { userId: "456" },
    });

    const serialized = error.toSerializable();

    assertEquals(serialized.id, error.id);
    assertEquals(serialized.type, "AuthError");
    assertEquals(serialized.message, "Auth failed: forbidden");
    assertEquals(serialized.reason, "forbidden");
    assertEquals(serialized.context, { userId: "456" });
  });

  await t.step("decoding tolerates additive reason values", () => {
    const value = RemoteError.parseJSON(JSON.stringify({
      id: "err_additive",
      type: "AuthError",
      message: "The request conflicts with current authentication state.",
      reason: "conflict",
    })).take();
    assert(!Result.isErr(value), "Expected additive reason to decode");
    assertEquals(value.type, "AuthError");
    if (value.type === "AuthError") {
      assertEquals("reason" in value, true);
      if ("reason" in value) assertEquals(value.reason, "conflict");
    }
  });
});

Deno.test("ValidationError", async (t) => {
  await t.step("serialization transforms errors to issues", () => {
    const original = new ValidationError({
      errors: [
        {
          path: "/email",
          message: "Invalid email format",
        },
        {
          path: "/password",
          message: "Password too short",
        },
      ],
      context: { requestId: "req-456" },
    });

    const serialized = original.toSerializable();

    assertEquals(serialized.issues.length, 2);
    assertEquals(serialized.issues[0], {
      path: "/email",
      message: "Invalid email format",
    });
    assertEquals(serialized.issues[1], {
      path: "/password",
      message: "Password too short",
    });
  });

  await t.step("serialization and validation", () => {
    const original = new ValidationError({
      errors: [
        {
          path: "/name",
          message: "Required field",
        },
      ],
    });

    const json = original.toJSON();
    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assertEquals(value.type, "ValidationError");
    if (value.type === "ValidationError" && "issues" in value) {
      assertEquals(value.id, original.id);
      assertEquals(value.issues.length, 1);
      assertEquals(value.issues[0].path, "/name");
      assertEquals(value.issues[0].message, "Required field");
    }
  });
});

Deno.test("RemoteError", async (t) => {
  await t.step("wraps validated remote error", () => {
    const remoteError = new AuthError({
      reason: "invalid_request",
    });

    const json = remoteError.toJSON();
    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const original = new RemoteError({
      error: value,
      context: { service: "auth" },
    });

    assertEquals(original.remoteError.type, "AuthError");
    if (
      original.remoteError.type === "AuthError" &&
      "reason" in original.remoteError
    ) {
      assertEquals(original.remoteError.reason, "invalid_request");
    }

    const serialized = original.toSerializable();
    assertEquals(serialized.context, { service: "auth" });
    assertEquals(serialized.remoteError.type, "AuthError");
  });

  await t.step("serialization and validation", () => {
    const remoteError = new ValidationError({
      errors: [{ path: "/userId", message: "Required field" }],
    });
    const json = remoteError.toJSON();
    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    const wrapper = new RemoteError({
      error: value,
    });

    const wrapperJson = wrapper.toJSON();
    const parsed = JSON.parse(wrapperJson);

    assertEquals(parsed.type, "RemoteError");
    assertEquals(parsed.remoteError.type, "ValidationError");
    assertEquals(parsed.remoteError.issues[0].path, "/userId");
  });
});

Deno.test("Operation lifecycle errors", async (t) => {
  await t.step(
    "OperationNotFoundError serialization preserves operation id",
    () => {
      const error = new OperationNotFoundError({
        operationId: "op-123",
        context: { requestId: "req-123" },
      });

      const serialized = error.toSerializable();

      assertEquals(serialized.id, error.id);
      assertEquals(serialized.type, "OperationNotFoundError");
      assertEquals(serialized.operationId, "op-123");
      assertEquals(serialized.context, { requestId: "req-123" });
    },
  );

  await t.step(
    "OperationAlreadyTerminalError serialization preserves optional operation fields",
    () => {
      const error = new OperationAlreadyTerminalError({
        operationId: "op-456",
        state: "failed",
        operation: "refund",
        service: "billing",
      });

      const serialized = error.toSerializable();

      assertEquals(serialized.type, "OperationAlreadyTerminalError");
      assertEquals(serialized.operationId, "op-456");
      assertEquals(serialized.state, "failed");
      assertEquals(serialized.operation, "refund");
      assertEquals(serialized.service, "billing");
    },
  );

  await t.step(
    "OperationMismatchError serialization preserves expected and actual identity",
    () => {
      const error = new OperationMismatchError({
        operationId: "op-789",
        expectedService: "billing",
        expectedOperation: "refund",
        actualService: "billing",
      });

      const serialized = error.toSerializable();

      assertEquals(serialized.type, "OperationMismatchError");
      assertEquals(serialized.operationId, "op-789");
      assertEquals(serialized.expectedService, "billing");
      assertEquals(serialized.expectedOperation, "refund");
      assertEquals(serialized.actualService, "billing");
      assertEquals(serialized.actualOperation, undefined);
    },
  );
});

Deno.test("Error - instance properties appear in serialization", () => {
  const error = new AuthError({
    reason: "invalid_request",
    context: { userId: "123" },
  });

  const serialized = error.toSerializable();

  assertEquals(serialized.id, error.id);
  assertEquals(serialized.type, "AuthError");
  assertEquals(serialized.message, "Auth failed: invalid_request");
  assertEquals(serialized.reason, "invalid_request");
  assertEquals(serialized.context, { userId: "123" });
});

Deno.test("machineErrorCode reads every Trellis error shape", async (t) => {
  await t.step("generated declared error carries its code under data", () => {
    assertEquals(
      machineErrorCode({
        name: "AuthError",
        data: { code: "approval_required", message: "approve" },
      }),
      "approval_required",
    );
  });

  await t.step("built-in error carries its code as reason", () => {
    assertEquals(
      machineErrorCode({ reason: "session_not_found" }),
      "session_not_found",
    );
  });

  await t.step("undecoded remote error carries its code on remoteError", () => {
    assertEquals(
      machineErrorCode({
        remoteError: { type: "trellis.auth@v1::AuthError", code: "conflict" },
      }),
      "conflict",
    );
  });

  await t.step("a top-level code wins over nested shapes", () => {
    assertEquals(
      machineErrorCode({ code: "revision_conflict", data: { code: "other" } }),
      "revision_conflict",
    );
  });

  await t.step("unrecognized values have no code", () => {
    assertEquals(machineErrorCode(undefined), undefined);
    assertEquals(machineErrorCode("boom"), undefined);
    assertEquals(machineErrorCode({ message: "no code here" }), undefined);
  });
});
