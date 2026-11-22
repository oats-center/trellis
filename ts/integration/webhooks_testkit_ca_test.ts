/**
 * A sender's HTTPS client that trusts only `runtime.caCertificate` reaches a
 * webhook URL of a testkit runtime, and one that trusts another CA does not.
 */

import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { request } from "node:https";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { repoTrellisSource, withTrellisRuntime } from "./_support/runtime.ts";

/** POST to `url` trusting only `ca`; the status, or the TLS error's code. */
function post(
  url: string,
  ca: string,
  headers: Record<string, string>,
): Promise<number | string> {
  return new Promise((resolve) => {
    const sent = request(url, { method: "POST", ca, headers }, (response) => {
      response.resume();
      resolve(response.statusCode ?? 0);
    });
    sent.on("error", (error: NodeJS.ErrnoException) =>
      resolve(error.code ?? error.message),
    );
    sent.end("{}");
  });
}

test("a client trusting only the runtime's CA reaches a webhook URL, and one trusting another CA does not", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "billing",
      deployment: "billing",
      contract: participants.WebhookService.participant,
      config: {
        values: { partnerUser: "partner", relayRanges: ["127.0.0.0/8"] },
        secrets: {
          signing: "whsec_signing-secret",
          hookToken: "hook-token-value",
          partnerPassword: "partner-password-value",
        },
      },
    });
    const { endpoints } = await runtime.callAdminRpc("webhooksEndpointsList", {
      service: key.deploymentId,
    });
    const url = endpoints.find((entry) => entry.endpoint === "token")?.url;
    assert.ok(url, "the token endpoint is listed");

    // Any HTTP status means the TLS handshake passed; the endpoint may still
    // be loading the secrets just set.
    const reached = await post(url, runtime.caCertificate, {
      "x-hook-token": "hook-token-value",
    });
    assert.equal(typeof reached, "number", `TLS refused: ${reached}`);

    // Another installation's freshly issued CA vouches for nothing here.
    const other = await mkdtemp(join(tmpdir(), "trellis-other-ca-"));
    try {
      await promisify(execFile)(repoTrellisSource().server, [
        "tls",
        "issue",
        "--out",
        other,
        "--host",
        "localhost",
      ]);
      const otherCa = await readFile(join(other, "ca.crt"), "utf8");
      assert.equal(
        await post(url, otherCa, { "x-hook-token": "hook-token-value" }),
        "SELF_SIGNED_CERT_IN_CHAIN",
      );
    } finally {
      await rm(other, { recursive: true, force: true });
    }
  });
});
