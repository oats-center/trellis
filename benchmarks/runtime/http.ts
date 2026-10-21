import { once } from "node:events";
import { createReadStream, createWriteStream } from "node:fs";
import { writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { text } from "node:stream/consumers";
import { pipeline } from "node:stream/promises";
import { streamDigest, WorkerOptions } from "./model.ts";

const options = WorkerOptions.parse(JSON.parse(await text(process.stdin)));
const http = createServer((request, response) => {
  void (async () => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
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
      const input =
        request.method === "GET"
          ? { value: url.searchParams.get("value") ?? "" }
          : JSON.parse(await text(request));
      response.writeHead(200, {
        "content-type": "application/json",
        "cache-control": "no-store",
      });
      response.end(JSON.stringify(input));
      return;
    }
    const size = Number(url.searchParams.get("size"));
    if (!options.sizes.includes(size)) {
      response.writeHead(400).end("Unknown payload");
      return;
    }
    if (url.pathname === "/download") {
      response.writeHead(200, {
        "content-length": String(size),
        "cache-control": "no-store",
      });
      await pipeline(
        createReadStream(`${options.output}/body-${size}.bin`),
        response,
      );
      return;
    }
    if (url.pathname === "/upload" && request.method === "PUT") {
      await pipeline(
        request,
        createWriteStream(`${options.output}/http-upload.bin`),
      );
      const stored = await streamDigest(
        createReadStream(`${options.output}/http-upload.bin`),
      );
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(stored));
      return;
    }
    response.writeHead(404).end("Not found");
  })().catch((error: unknown) =>
    response.destroy(error instanceof Error ? error : undefined),
  );
});
http.listen(0, "127.0.0.1");
await once(http, "listening");
const address = http.address();
if (!address || typeof address === "string")
  throw new Error("HTTP listener has no TCP address");
await writeFile(
  `${options.output}/http.json`,
  JSON.stringify({
    pid: process.pid,
    httpUrl: `http://127.0.0.1:${address.port}`,
    authentication: "none",
    protocol: "HTTP/1.1 plaintext keep-alive",
    storage: "local filesystem; no per-upload fsync",
  }),
);
await once(process, "SIGTERM");
await new Promise<void>((resolve, reject) =>
  http.close((error) => (error ? reject(error) : resolve())),
);
