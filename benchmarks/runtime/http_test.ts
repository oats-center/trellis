import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { request as httpsRequest } from "node:https";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { text } from "node:stream/consumers";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { spawnTracked } from "../../ts/integration/_support/process.ts";
import { waitFor } from "../../ts/packages/trellis-testkit/src/wait.ts";
import { createHttpClient } from "./http_client.ts";
import { generateHttpTls, loadHttpTls, signRequest } from "./http_security.ts";
import { deadline, payload, payloadChunks } from "./model.ts";

const size = 23_001;
const digest = createHash("sha256").update(payload(size)).digest("hex");

/** Start the Node HTTP baseline without a Trellis identity (no store mode). */
async function startServer(output: string, httpTlsDir?: string) {
  const child = spawnTracked(process.execPath, [
    "--experimental-transform-types",
    fileURLToPath(new URL("./http.ts", import.meta.url)),
  ]);
  const stderr: Buffer[] = [];
  child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk));
  child.stdin.end(
    JSON.stringify({
      role: "provider",
      output,
      trellisUrl: "http://127.0.0.1",
      username: "admin",
      password: "unused",
      samples: 1,
      calls: 1,
      sizes: [size],
      sessionCounts: [1],
      idleSeconds: 1,
      cpuTicks: 100,
      arrivalRate: 0,
      maxOutstanding: 1,
      httpChunkBytes: 4096,
      ...(httpTlsDir ? { httpTlsDir } : {}),
    }),
  );
  const { httpUrl } = await waitFor(
    async () => {
      if (child.exitCode !== null)
        throw new Error(Buffer.concat(stderr).toString());
      return z
        .object({ httpUrl: z.url() })
        .parse(JSON.parse(await readFile(join(output, "http.json"), "utf8")));
    },
    { timeoutMs: 5_000, intervalMs: 20 },
  );
  return {
    httpUrl,
    async stop() {
      child.kill("SIGTERM");
      try {
        const status = await deadline(child.exited, 5_000);
        assert.equal(status.code, 0, Buffer.concat(stderr).toString());
      } finally {
        child.kill("SIGKILL");
        await child.exited.catch(() => undefined);
      }
    },
  };
}

test(
  "Node HTTP baseline serves raw and stream transfers with verified and durable boundaries",
  { timeout: 15_000 },
  async () => {
    const output = await mkdtemp(join(tmpdir(), "trellis-http-benchmark-"));
    const server = await startServer(output);
    const http = createHttpClient(server.httpUrl, undefined);
    try {
      assert.deepEqual(
        JSON.parse(
          await text(await http.send("GET", "/echo?value=round-trip")),
        ),
        { value: "round-trip" },
      );
      for (const mode of ["raw", "stream"] as const) {
        const download = await http.send(
          "GET",
          `/download/${mode}?size=${size}`,
        );
        assert.equal(download.statusCode, 200);
        const hash = createHash("sha256");
        for await (const chunk of download as AsyncIterable<Uint8Array>)
          hash.update(chunk);
        assert.equal(hash.digest("hex"), digest, mode);
        const started = Date.now();
        const upload = await http.send("PUT", `/upload/${mode}?size=${size}`, {
          chunks: payloadChunks(size, 4096),
          length: size,
        });
        const stored = JSON.parse(await text(upload));
        assert.equal(stored.size, size, mode);
        assert.equal(stored.digest, digest, mode);
        assert(stored.verifiedUnixMs >= started, mode);
        assert(stored.durableUnixMs >= started, mode);
      }
      const store = await http.send("GET", `/download/store?size=${size}`);
      store.resume();
      assert.equal(store.statusCode, 503);
      const live = await http.send("GET", "/live?value=first-event");
      assert.deepEqual(JSON.parse(await text(live)), { value: "first-event" });
    } finally {
      http.close();
      await server.stop();
      await rm(output, { recursive: true, force: true });
    }
  },
);

test(
  "TLS HTTP baseline requires a client certificate and a fresh request signature",
  { timeout: 30_000 },
  async () => {
    const output = await mkdtemp(join(tmpdir(), "trellis-https-benchmark-"));
    const tlsDir = join(output, "tls");
    await generateHttpTls(tlsDir);
    const tls = await loadHttpTls(tlsDir);
    const server = await startServer(output, tlsDir);
    const url = new URL(server.httpUrl);
    assert.equal(url.protocol, "https:");
    const http = createHttpClient(server.httpUrl, tls);
    const raw = (
      target: string,
      headers: Record<string, string>,
      identity = true,
    ) =>
      new Promise<number>((resolve, reject) => {
        const request = httpsRequest(
          {
            host: url.hostname,
            port: url.port,
            path: target,
            agent: false,
            ca: tls.ca,
            ...(identity ? { cert: tls.clientCert, key: tls.clientKey } : {}),
            headers,
          },
          (response) => {
            response.resume();
            resolve(response.statusCode ?? 0);
          },
        );
        request.once("error", reject);
        request.end();
      });
    try {
      // Signed upload and download round trip over mutual TLS.
      const upload = await http.send("PUT", `/upload/stream?size=${size}`, {
        chunks: payloadChunks(size, 4096),
        length: size,
      });
      assert.equal(JSON.parse(await text(upload)).digest, digest);
      const target = "/echo?value=signed";
      assert.equal(
        JSON.parse(await text(await http.send("GET", target))).value,
        "signed",
      );
      // A certificate alone does not authorize a request.
      assert.equal(await raw(target, {}), 401);
      // A signature is single-use and bound to its exact target.
      const proof = signRequest(tls, "GET", target, undefined);
      assert.equal(await raw(target, proof), 200);
      assert.equal(await raw(target, proof), 401);
      assert.equal(
        await raw(
          "/echo?value=other",
          signRequest(tls, "GET", target, undefined),
        ),
        401,
      );
      // Without a client certificate the handshake itself is refused.
      await assert.rejects(
        raw(target, signRequest(tls, "GET", target, undefined), false),
      );
    } finally {
      http.close();
      await server.stop();
      await rm(output, { recursive: true, force: true });
    }
  },
);
