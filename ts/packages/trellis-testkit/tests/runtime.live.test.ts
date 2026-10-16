import assert from "node:assert/strict";
import { lstat } from "node:fs/promises";
import { test } from "node:test";
import { ulid } from "ulid";
import { TrellisTestRuntime } from "../index.ts";

test(
  "password-setup link admits a Node administrator and credentials survive restart",
  { timeout: 90_000 },
  async () => {
    const cli = process.env.TRELLIS_TEST_CLI_BIN;
    const server = process.env.TRELLIS_TEST_SERVER_BIN;
    assert(cli && server);
    const runtime = await TrellisTestRuntime.start({
      firstAdmin: "browser-flow",
      trellis: { source: { kind: "path", cli, server } },
    });
    try {
      const before = await runtime.callAdminRpc("authUsersResolve", {});
      await runtime.restart();
      const after = await runtime.callAdminRpc("authUsersResolve", {});
      assert.equal(
        after.user.principal.principalId,
        before.user.principal.principalId,
      );
    } finally {
      await runtime.stop();
    }
  },
);

test(
  "Node native administration survives a real runtime restart and cleans up its sandbox",
  { timeout: 90_000 },
  async () => {
    const cli = process.env.TRELLIS_TEST_CLI_BIN;
    const server = process.env.TRELLIS_TEST_SERVER_BIN;
    assert.ok(
      cli && server,
      "provide the matching producer-built CLI and server binaries",
    );
    const runtime = await TrellisTestRuntime.start({
      trellis: { source: { kind: "path", cli, server } },
    });
    try {
      const username = `node-harness-${ulid()}`;
      const input = {
        username,
        displayName: "Node lifecycle user",
        password: `native-password-${ulid()}`,
        idempotencyKey: ulid(),
      };
      const { user } = await runtime.callAdminRpc("authUsersCreate", input);
      await runtime.restart();
      const retained = await runtime.callAdminRpc("authUsersCreate", input);
      assert.equal(retained.user.username, user.username);
      assert.equal(
        retained.user.principal.principalId,
        user.principal.principalId,
      );
    } finally {
      await runtime.stop();
    }
    await assert.rejects(() => lstat(runtime.workdir), { code: "ENOENT" });
  },
);
