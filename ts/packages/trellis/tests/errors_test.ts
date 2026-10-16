import assert from "node:assert/strict";
import { test } from "node:test";
import {
  AuthError,
  machineErrorCode,
  RemoteError,
  ValidationError,
} from "../errors/index.ts";
import { Result } from "../../result/mod.ts";

test("AuthError", async (t) => {
  await t.test("serialization and validation", () => {
    const original = new AuthError({
      reason: "invalid_request",
      context: { userId: "123", endpoint: "/api/users" },
    });

    const json = original.toJSON();

    const result = RemoteError.parseJSON(json);
    const value = result.take();
    assert(!Result.isErr(value), "Expected successful parse");

    assert.deepEqual(value.type, "AuthError");
    if (value.type === "AuthError" && "reason" in value) {
      assert.deepEqual(value.id, original.id);
      assert.deepEqual(value.reason, "invalid_request");
      assert.deepEqual(value.context, {
        userId: "123",
        endpoint: "/api/users",
      });
    }
  });

  await t.test("decoding tolerates additive reason values", () => {
    const value = RemoteError.parseJSON(
      JSON.stringify({
        id: "err_additive",
        type: "AuthError",
        message: "The request conflicts with current authentication state.",
        reason: "conflict",
      }),
    ).take();
    assert(!Result.isErr(value), "Expected additive reason to decode");
    assert.deepEqual(value.type, "AuthError");
    if (value.type === "AuthError") {
      assert.deepEqual("reason" in value, true);
      if ("reason" in value) assert.deepEqual(value.reason, "conflict");
    }
  });
});

test("ValidationError", async (t) => {
  await t.test("serialization transforms errors to issues", () => {
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

    assert.deepEqual(serialized.issues.length, 2);
    assert.deepEqual(serialized.issues[0], {
      path: "/email",
      message: "Invalid email format",
    });
    assert.deepEqual(serialized.issues[1], {
      path: "/password",
      message: "Password too short",
    });
  });

  await t.test("serialization and validation", () => {
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

    assert.deepEqual(value.type, "ValidationError");
    if (value.type === "ValidationError" && "issues" in value) {
      assert.deepEqual(value.id, original.id);
      assert.deepEqual(value.issues.length, 1);
      assert.deepEqual(value.issues[0].path, "/name");
      assert.deepEqual(value.issues[0].message, "Required field");
    }
  });
});

test("RemoteError", async (t) => {
  await t.test("wraps validated remote error", () => {
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

    assert.deepEqual(original.remoteError.type, "AuthError");
    if (
      original.remoteError.type === "AuthError" &&
      "reason" in original.remoteError
    ) {
      assert.deepEqual(original.remoteError.reason, "invalid_request");
    }

    const serialized = original.toSerializable();
    assert.deepEqual(serialized.context, { service: "auth" });
    assert.deepEqual(serialized.remoteError.type, "AuthError");
  });

  await t.test("serialization and validation", () => {
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

    assert.deepEqual(parsed.type, "RemoteError");
    assert.deepEqual(parsed.remoteError.type, "ValidationError");
    assert.deepEqual(parsed.remoteError.issues[0].path, "/userId");
  });
});

test("machineErrorCode reads every Trellis error shape", async (t) => {
  await t.test("generated declared error carries its code under data", () => {
    assert.deepEqual(
      machineErrorCode({
        name: "AuthError",
        data: { code: "approval_required", message: "approve" },
      }),
      "approval_required",
    );
  });

  await t.test("built-in error carries its code as reason", () => {
    assert.deepEqual(
      machineErrorCode({ reason: "session_not_found" }),
      "session_not_found",
    );
  });

  await t.test("undecoded remote error carries its code on remoteError", () => {
    assert.deepEqual(
      machineErrorCode({
        remoteError: { type: "trellis.auth@v1::AuthError", code: "conflict" },
      }),
      "conflict",
    );
  });

  await t.test("a top-level code wins over nested shapes", () => {
    assert.deepEqual(
      machineErrorCode({ code: "revision_conflict", data: { code: "other" } }),
      "revision_conflict",
    );
  });

  await t.test("unrecognized values have no code", () => {
    assert.deepEqual(machineErrorCode(undefined), undefined);
    assert.deepEqual(machineErrorCode("boom"), undefined);
    assert.deepEqual(machineErrorCode({ message: "no code here" }), undefined);
  });
});
