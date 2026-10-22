import { createHash } from "node:crypto";
import { once } from "node:events";
import { createReadStream, createWriteStream } from "node:fs";
import { writeFile } from "node:fs/promises";
import {
  createServer as createHttpServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { Readable } from "node:stream";
import { text } from "node:stream/consumers";
import { pipeline } from "node:stream/promises";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./packages/performance-trellis/index.js";
import { loadHttpTls, requestVerifier } from "./http_security.ts";
import {
  HTTP_MODES,
  type HttpMode,
  payloadChunks,
  streamDigest,
  WorkerOptions,
} from "./model.ts";

const options = WorkerOptions.parse(JSON.parse(await text(process.stdin)));
const chunk = options.httpChunkBytes;
const tls = options.httpTlsDir
  ? await loadHttpTls(options.httpTlsDir)
  : undefined;
const verifyRequest = tls ? requestVerifier(tls) : undefined;
// Store mode writes to and reads from the provider's own `files` store, through
// the same identity and store API the Trellis provider uses. No handlers are
// mounted, so this process never serves Trellis traffic. Without a service
// identity (the infrastructure-free unit test) store mode answers 503.
const service = options.serviceSeed
  ? await TrellisService.connect({
      trellisUrl: options.trellisUrl,
      name: "benchmark-http",
      participant: participants.Provider.participant,
      seed: options.serviceSeed,
    }).orThrow()
  : undefined;
const store = service ? await service.store.files.open().orThrow() : undefined;
for (const size of options.sizes) {
  await pipeline(
    Readable.from(payloadChunks(size, chunk)),
    createWriteStream(`${options.output}/body-${size}.bin`, {
      highWaterMark: chunk,
    }),
  );
}
/** Wall clock with sub-millisecond resolution, comparable across processes. */
const wall = () => performance.timeOrigin + performance.now();

async function upload(
  mode: HttpMode,
  size: number,
  request: IncomingMessage,
): Promise<{
  size: number;
  digest: string;
  verifiedUnixMs: number;
  durableUnixMs: number;
}> {
  if (mode === "stream") {
    const stored = await streamDigest(request);
    const at = wall();
    return { ...stored, verifiedUnixMs: at, durableUnixMs: at };
  }
  if (mode === "store") {
    if (!store) throw new Error("store mode requires a service identity");
    const hash = createHash("sha256");
    let length = 0;
    let verifiedUnixMs = 0;
    const body = (async function* () {
      for await (const part of request as AsyncIterable<Uint8Array>) {
        hash.update(part);
        length += part.length;
        yield part;
      }
      verifiedUnixMs = wall();
    })();
    await store.put(`http-upload-${size}`, body).orThrow();
    return {
      size: length,
      digest: hash.digest("hex"),
      verifiedUnixMs,
      durableUnixMs: wall(),
    };
  }
  // Raw: idiomatic file write (closed, not fsynced), then a verifying re-read.
  const path = `${options.output}/http-upload.bin`;
  await pipeline(request, createWriteStream(path, { highWaterMark: chunk }));
  const durableUnixMs = wall();
  const stored = await streamDigest(
    createReadStream(path, { highWaterMark: chunk }),
  );
  return { ...stored, verifiedUnixMs: wall(), durableUnixMs };
}

async function download(
  mode: HttpMode,
  size: number,
  response: ServerResponse,
): Promise<void> {
  if (mode === "store") {
    if (!store) throw new Error("store mode requires a service identity");
    const entry = await store.get(String(size)).orThrow();
    response.writeHead(200, {
      "content-length": String(entry.info.size),
      "cache-control": "no-store",
      "x-bench-digest": entry.info.digest ?? "",
    });
    await pipeline(Readable.from(await entry.stream().orThrow()), response);
    return;
  }
  response.writeHead(200, {
    "content-length": String(size),
    "cache-control": "no-store",
  });
  await pipeline(
    mode === "raw"
      ? createReadStream(`${options.output}/body-${size}.bin`, {
          highWaterMark: chunk,
        })
      : Readable.from(payloadChunks(size, chunk)),
    response,
  );
}

const handler = (request: IncomingMessage, response: ServerResponse) => {
  void (async () => {
    const target = request.url ?? "/";
    const failure = verifyRequest?.(
      request.method ?? "GET",
      target,
      request.headers,
    );
    if (failure) {
      request.resume();
      response.writeHead(401).end(failure);
      return;
    }
    const url = new URL(target, "http://127.0.0.1");
    if (url.pathname === "/live") {
      response.writeHead(200, {
        "content-type": "application/x-ndjson",
        "cache-control": "no-store",
      });
      response.end(
        JSON.stringify({ value: url.searchParams.get("value") ?? "" }) + "\n",
      );
      return;
    }
    if (url.pathname === "/echo") {
      response.writeHead(200, {
        "content-type": "application/json",
        "cache-control": "no-store",
      });
      response.end(
        JSON.stringify({ value: url.searchParams.get("value") ?? "" }),
      );
      return;
    }
    const route = /^\/(upload|download)\/([a-z]+)$/.exec(url.pathname);
    const mode = HTTP_MODES.find((candidate) => candidate === route?.[2]);
    const size = Number(url.searchParams.get("size"));
    if (!route || !mode || !options.sizes.includes(size)) {
      request.resume();
      response.writeHead(404).end("Not found");
      return;
    }
    if (mode === "store" && !store) {
      request.resume();
      response.writeHead(503).end("Store mode requires a service identity");
      return;
    }
    if (route[1] === "download") {
      await download(mode, size, response);
      return;
    }
    if (request.method !== "PUT") {
      response.writeHead(405).end("Method not allowed");
      return;
    }
    const stored = await upload(mode, size, request);
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(stored));
  })().catch((error: unknown) =>
    response.destroy(error instanceof Error ? error : undefined),
  );
};
const http = tls
  ? createHttpsServer(
      {
        key: tls.serverKey,
        cert: tls.serverCert,
        ca: tls.ca,
        requestCert: true,
        rejectUnauthorized: true,
      },
      handler,
    )
  : createHttpServer(handler);
http.listen(0, "127.0.0.1");
await once(http, "listening");
const address = http.address();
if (!address || typeof address === "string")
  throw new Error("HTTP listener has no TCP address");
await writeFile(
  `${options.output}/http.json`,
  JSON.stringify({
    pid: process.pid,
    httpUrl: `${tls ? "https" : "http"}://127.0.0.1:${address.port}`,
    implementation: "Node node:http/node:https",
    authentication: tls
      ? "mutual TLS plus per-request Ed25519 signature"
      : "none",
    protocol: tls
      ? "HTTP/1.1 over TLS keep-alive"
      : "HTTP/1.1 plaintext keep-alive",
    chunkBytes: chunk,
    modes: {
      raw: "local file; closed, not fsynced; verifying re-read",
      stream: "hashed inline and discarded",
      store: "hashed inline into the provider's NATS Object Store",
    },
  }),
);
await once(process, "SIGTERM");
await new Promise<void>((resolve, reject) =>
  http.close((error) => (error ? reject(error) : resolve())),
);
await service?.stop();
