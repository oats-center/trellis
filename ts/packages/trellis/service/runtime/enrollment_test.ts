import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";

import {
  enrollServiceIdentity,
  newIdentitySeed,
  serviceIdentityStatus,
} from "./enrollment.ts";

async function listen(server: Server): Promise<string> {
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
}

test("enrollment and identity requests refuse a redirect and send nothing to its target", async () => {
  const received: string[] = [];
  const elsewhere = createServer((request, response) => {
    received.push(`${request.method} ${request.url}`);
    response.end("{}");
  });
  const elsewhereUrl = await listen(elsewhere);
  let status = 307;
  const trellis = createServer((request, response) => {
    request.resume();
    response.writeHead(status, { location: `${elsewhereUrl}${request.url}` });
    response.end();
  });
  const trellisUrl = await listen(trellis);
  try {
    const seed = newIdentitySeed();
    await assert.rejects(
      enrollServiceIdentity({
        trellisUrl,
        origin: trellisUrl,
        secret: "trellis-enroll-secret",
        seed,
        participantId: "acme.Orders",
        participantDigest: "digest",
        title: "pod",
      }),
      new RegExp(
        `/auth/service/enroll with a 307 redirect to ${elsewhereUrl}/auth/service/enroll`,
      ),
    );
    status = 308;
    await assert.rejects(
      serviceIdentityStatus({ trellisUrl, origin: trellisUrl, seed }),
      new RegExp(
        `/auth/service/identity with a 308 redirect to ${elsewhereUrl}/auth/service/identity`,
      ),
    );
    assert.deepEqual(received, []);
  } finally {
    trellis.close();
    elsewhere.close();
  }
});
