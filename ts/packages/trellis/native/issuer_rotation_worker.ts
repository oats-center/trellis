import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { apps } from "../../trellis-testkit/trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { NativeOAuthClient, type NativeOAuthSession } from "../native.ts";

const [mode, directory, configPath] = process.argv.slice(2);
assert.ok(mode && directory && configPath);
function summary(session: NativeOAuthSession) {
  return {
    scope: session.scope,
    trust: session.trust,
    principalId: session.principalId,
    grantId: session.oauthGrantId,
    loginId: session.loginSessionId,
    dpop: session.durableDpopJkt,
    runtime: session.runtimeId,
    nkey: session.auth.sessionNkey,
  };
}

if (mode === "login") {
  const issuer = process.env.TRELLIS_ROTATION_ISSUER;
  assert.ok(issuer);
  const { session } = await loginNativeTestClient({
    issuer,
    directory,
    clientId: "trellis.cli",
    request: apps.cli.request,
    approvedCapabilities: apps.cli.request.requiredCapabilities,
    username: "admin",
    password: "a strong bootstrap password",
  });
  console.log(JSON.stringify(summary(session)));
  await session.close();
} else {
  const previous = JSON.parse(await readFile(configPath, "utf8"));
  const client = (
    await NativeOAuthClient.open({
      directory,
      scope: previous.scope,
      trust: previous.trust,
    })
  ).orThrow();
  try {
    const resumed = await client.resume();
    if (mode === "denied") {
      assert.equal(resumed.isErr(), true);
      if (resumed.isErr())
        assert.equal(resumed.error.reason, "OAuthTrustUnavailable");
      console.log(JSON.stringify({ denied: true }));
    } else {
      const session = resumed.orThrow();
      const installed = summary(session);
      for (const key of ["principalId", "grantId", "loginId", "dpop"] as const)
        assert.equal(installed[key], previous[key]);
      assert.notEqual(installed.runtime, previous.runtime);
      assert.notEqual(installed.nkey, previous.nkey);
      if (mode === "inflight") {
        console.log(JSON.stringify({ ready: true }));
        const lines = createInterface({ input: process.stdin });
        await lines[Symbol.asyncIterator]().next();
        lines.close();
        const refreshed = await session.refresh();
        assert.equal(refreshed.isErr(), true);
        if (refreshed.isErr())
          assert.equal(refreshed.error.reason, "OAuthTrustUnavailable");
        console.log(JSON.stringify({ preserved: true }));
      } else {
        console.log(JSON.stringify(installed));
      }
    }
  } finally {
    await client.close();
  }
}
