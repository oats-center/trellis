import { digest, WorkerOptions } from "./model.ts";

const options = WorkerOptions.parse(
  JSON.parse(await new Response(Deno.stdin.readable).text()),
);
const http = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen: () => {} },
  async (request) => {
    const url = new URL(request.url);
    if (url.pathname === "/live") {
      const value = url.searchParams.get("value") ?? "";
      return new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(
              new TextEncoder().encode(JSON.stringify({ value }) + "\n"),
            );
            controller.close();
          },
        }),
        {
          headers: {
            "content-type": "application/x-ndjson",
            "cache-control": "no-store",
          },
        },
      );
    }
    if (url.pathname === "/echo") {
      const input = request.method === "GET"
        ? { value: url.searchParams.get("value") ?? "" }
        : await request.json();
      return Response.json(input, { headers: { "cache-control": "no-store" } });
    }
    const size = Number(url.searchParams.get("size"));
    if (!options.sizes.includes(size)) {
      return new Response("Unknown payload", {
        status: 400,
      });
    }
    if (url.pathname === "/download") {
      const file = await Deno.open(`${options.output}/body-${size}.bin`);
      return new Response(file.readable, {
        headers: {
          "content-length": String(size),
          "cache-control": "no-store",
        },
      });
    }
    if (url.pathname === "/upload" && request.body) {
      const file = await Deno.open(`${options.output}/http-upload.bin`, {
        write: true,
        create: true,
        truncate: true,
      });
      await request.body.pipeTo(file.writable);
      const body = await Deno.readFile(`${options.output}/http-upload.bin`);
      return Response.json({ size: body.length, digest: await digest(body) });
    }
    return new Response("Not found", { status: 404 });
  },
);
await Deno.writeTextFile(
  `${options.output}/http.json`,
  JSON.stringify({
    pid: Deno.pid,
    httpUrl: `http://127.0.0.1:${http.addr.port}`,
    authentication: "none",
    protocol: "HTTP/1.1 plaintext keep-alive",
    storage: "local filesystem; no per-upload fsync",
  }),
);
const stopped = Promise.withResolvers<void>();
Deno.addSignalListener("SIGTERM", () => stopped.resolve());
try {
  await stopped.promise;
} finally {
  await http.shutdown();
}
