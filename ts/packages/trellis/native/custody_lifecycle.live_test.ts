import assert from "node:assert/strict";
import { test } from "node:test";

import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { apps } from "../../trellis-testkit/trellis/index.js";
import {
  MemoryNativeOAuthCustody,
  NativeOAuthClient,
  type NativeOAuthCustody,
  connectNativeClient,
  connectNativeSession,
} from "../native.ts";

for (const scenario of [
  "explicit",
  "release-failure",
  "construction-failure",
  "terminal",
]) {
  const failRelease = scenario === "release-failure" || scenario === "terminal";
  test(
    `native custody disposal joins ${scenario} and permits exclusive reacquisition`,
    { timeout: 60_000 },
    async () => {
      await withTrellisRuntime(async (runtime) => {
        const memory = new MemoryNativeOAuthCustody();
        const release = Promise.withResolvers<void>();
        const failure = new Error("application custody release failed");
        let releases = 0;
        const custody: NativeOAuthCustody = {
          async acquire(...args) {
            const lease = await memory.acquire(...args);
            let closing: Promise<void> | undefined;
            return new Proxy(lease, {
              get(target, key) {
                if (key === "close")
                  return () =>
                    (closing ??= (async () => {
                      releases++;
                      await release.promise;
                      await lease.close();
                      if (failRelease) throw failure;
                    })());
                const value = Reflect.get(target, key, target);
                return typeof value === "function" ? value.bind(target) : value;
              },
            });
          },
        };
        const login = await loginNativeTestClient({
          issuer: runtime.trellisUrl,
          username: runtime.adminUsername,
          password: runtime.adminPassword,
          clientId: "trellis.cli",
          request: apps.cli.request,
          custody,
        });
        const trust = login.session.trust;
        const scope = login.session.scope;
        if (scenario === "construction-failure") {
          let settled = false;
          const connecting = connectNativeClient({
            ...login,
            request: apps.cli.request,
            signal: AbortSignal.abort(new Error("cancel native construction")),
          }).take();
          void connecting.then(() => {
            settled = true;
          });
          try {
            await runtime.waitFor(() => releases === 1);
            assert.equal(settled, false);
            await assert.rejects(() => memory.acquire(scope, trust));
            release.resolve();
            assert((await connecting).isErr());
            const reacquired = await memory.acquire(scope, trust);
            await reacquired.close();
            assert.equal(releases, 1);
          } finally {
            release.resolve();
            await connecting;
          }
          return;
        }
        if (scenario === "terminal") {
          const connected = await connectNativeSession(login).orThrow();
          let settled = false;
          const ending = connected.closed();
          void ending.then(
            () => {
              settled = true;
            },
            () => {
              settled = true;
            },
          );
          try {
            // End the actual transport owner, not the composed native facade.
            await connected.manager.close();
            await runtime.waitFor(() => releases === 1);
            assert.equal(settled, false);
            await assert.rejects(() => memory.acquire(scope, trust));
            release.resolve();
            await assert.rejects(ending, failure);
            await assert.rejects(() => connected.close(), failure);
            const reacquired = await memory.acquire(scope, trust);
            await reacquired.close();
            assert.equal(releases, 1);
          } finally {
            release.resolve();
            await connected.close().catch(() => undefined);
          }
          return;
        }
        const client = await connectNativeClient({
          ...login,
          request: apps.cli.request,
        }).orThrow();
        try {
          await client.sessionsList({}).orThrow();
          let settled = false;
          const closing = Promise.allSettled([
            client.connection.close(),
            client.connection.close(),
          ]);
          void closing.then(() => {
            settled = true;
          });
          await runtime.waitFor(() => releases === 1);
          assert.equal(
            settled,
            false,
            "close must retain custody until its application releases it",
          );
          assert.equal(releases, 1);
          await assert.rejects(() => memory.acquire(scope, trust));
          release.resolve();
          const outcomes = await closing;
          for (const outcome of outcomes) {
            assert.equal(
              outcome.status,
              failRelease ? "rejected" : "fulfilled",
            );
            if (outcome.status === "rejected")
              assert.equal(outcome.reason, failure);
          }
          assert.equal(releases, 1);
          const reopened = (
            await NativeOAuthClient.open({
              custody: memory,
              scope,
              trust,
            })
          ).orThrow();
          await reopened.close();
          if (failRelease)
            await assert.rejects(() => client.connection.close(), failure);
          else await client.connection.close();
          assert.equal(releases, 1);
        } finally {
          release.resolve();
          await client.connection.close().catch(() => undefined);
        }
      });
    },
  );
}
