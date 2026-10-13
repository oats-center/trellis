import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";
import { chromium, type Page } from "playwright";
import { build } from "vite";

test(
  "browser State shares only its authenticated namespace and survives runtime and Chromium restart",
  { timeout: 180000 },
  async () => {
    const issuer = process.env.TRELLIS_TEST_OAUTH_ORIGIN!;
    const origins = JSON.parse(process.env.TRELLIS_STATE_ORIGINS!) as string[];
    const directory = await mkdtemp(join(tmpdir(), "trellis-state-"));
    await build({
      configFile: false,
      logLevel: "error",
      build: {
        rollupOptions: { output: { inlineDynamicImports: true } },
        outDir: directory,
        emptyOutDir: false,
        lib: {
          entry: new URL("./state_client.ts", import.meta.url).pathname,
          formats: ["es"],
          fileName: () => "client.js",
        },
      },
    });
    const source = await readFile(join(directory, "client.js"));
    const servers = origins.map(() =>
      createServer((req, res) => {
        res.setHeader(
          "Content-Type",
          req.url === "/client.js" ? "text/javascript" : "text/html",
        );
        res.end(
          req.url === "/client.js"
            ? source
            : "<!doctype html><title>State acceptance</title>",
        );
      }),
    );
    await Promise.all(
      servers.map(async (server, index) => {
        server.listen(Number(new URL(origins[index]).port), "127.0.0.1");
        await once(server, "listening");
      }),
    );
    let context = await chromium.launchPersistentContext(
      join(directory, "profile"),
      { headless: true },
    );
    const browser = await chromium.launch({ headless: true });
    const registrations: unknown[] = [];
    let lostReplies = 0;
    const dropReplies = new Set<string>();
    const fenceReply = Promise.withResolvers<void>();
    await context.routeWebSocket(/.*/, (socket) => {
      const server = socket.connectToServer();
      // NATS commands may span WebSocket frames or share a frame. Forward
      // complete commands unchanged, withholding only the first real reply.
      for (const [source, destination, outgoing] of [
        [socket, server, true],
        [server, socket, false],
      ] as const) {
        let buffered = Buffer.alloc(0);
        source.onMessage((message) => {
          buffered = Buffer.concat([buffered, Buffer.from(message)]);
          while (true) {
            const end = buffered.indexOf("\r\n");
            if (end < 0) return;
            const fields = buffered.subarray(0, end).toString().split(" ");
            const payload = ["PUB", "HPUB", "MSG", "HMSG"].includes(fields[0]);
            const size = payload ? Number(fields.at(-1)) : 0;
            const length = end + 2 + (payload ? size + 2 : 0);
            if (buffered.length < length) return;
            const command = buffered.subarray(0, length);
            buffered = buffered.subarray(length);
            if (outgoing && fields[1] === "rpc.v1.state.Register") {
              const headers = fields[0] === "HPUB" ? Number(fields.at(-2)) : 0;
              registrations.push(
                JSON.parse(
                  command
                    .subarray(end + 2 + headers, end + 2 + size)
                    .toString(),
                ),
              );
              if ([1, 3].includes(registrations.length))
                dropReplies.add(fields[2]);
            }
            if (!outgoing && dropReplies.delete(fields[1])) {
              lostReplies++;
              if (lostReplies === 2) fenceReply.resolve();
              continue;
            }
            destination.send(command);
          }
        });
      }
    });
    async function open(
      page: Page,
      origin: string,
      clientId = "state-browser",
      username = process.env.TRELLIS_TEST_ADMIN_USERNAME!,
      password = process.env.TRELLIS_TEST_ADMIN_PASSWORD!,
      resume = false,
    ) {
      page.setDefaultTimeout(20000);
      page.on("pageerror", (error) => console.error(error));
      await page.goto(origin);
      const connecting = page.evaluate(
        async ({ issuer, clientId }) => {
          const client = await import("/client.js");
          await client.connect(issuer, clientId);
        },
        { issuer, clientId },
      );
      if (!resume) {
        await connecting.catch((error) => {
          if (
            !String(error).includes("Execution context was destroyed") &&
            !String(error).includes("OAuthAuthorizationRequired")
          )
            throw error;
        });
        await page.getByLabel("Username", { exact: true }).fill(username);
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
          .waitFor();
        for (const box of await page.getByRole("checkbox").all())
          if (await box.isEnabled()) await box.check();
        await page
          .getByRole("button", { name: "Authorize application" })
          .click();
        await page.waitForURL(
          (url) => url.origin === origin && url.pathname === "/callback",
        );
        await page.evaluate(
          async ({ issuer, clientId }) => {
            await (await import("/client.js")).connect(issuer, clientId);
          },
          { issuer, clientId },
        );
      } else await connecting;
    }
    async function call(page: Page, method: string, ...args: string[]) {
      return page.evaluate(
        async ({ method, args }) => {
          const client = await import("/client.js");
          return client[method](...args);
        },
        { method, args },
      );
    }
    try {
      let first = await context.newPage();
      await open(first, origins[0]);
      assert.deepEqual(await call(first, "firstUseError"), [
        "trellis.request.failed",
        "trellis.request.failed",
      ]);
      assert.equal(lostReplies, 1);
      assert.equal(await call(first, "get"), null);
      assert.equal(registrations.length, 2);
      assert.deepEqual(registrations[1], registrations[0]);
      await call(first, "freshHandle");
      const oldAttempt = call(first, "firstUseError");
      await fenceReply.promise;
      await call(first, "register");
      assert.deepEqual(await oldAttempt, [
        "trellis.request.failed",
        "trellis.request.failed",
      ]);
      assert.equal(await call(first, "get"), null);
      assert.equal(
        registrations.length,
        4,
        "late failure must not clear the newer successful registration",
      );
      const original = await call(first, "create", "persisted");
      const second = await (await browser.newContext()).newPage();
      await open(second, origins[0]);
      assert.deepEqual(await call(second, "get"), original);
      await call(second, "watch");
      assert.deepEqual(await call(second, "next"), original);
      const update = call(second, "next");
      const results = await Promise.all([
        call(first, "replace", original.revision, "left"),
        call(second, "replace", original.revision, "right"),
      ]);
      assert.equal(results.filter((result) => result.conflict).length, 1);
      const winner = results.find((result) => !result.conflict).value;
      assert.deepEqual(
        results.find((result) => result.conflict).current,
        winner,
      );
      assert.deepEqual(await update, winner);
      for (const [origin, clientId, user, password] of [
        [
          origins[1],
          "state-browser",
          process.env.TRELLIS_TEST_ADMIN_USERNAME!,
          process.env.TRELLIS_TEST_ADMIN_PASSWORD!,
        ],
        [
          origins[0],
          "state-browser-other",
          process.env.TRELLIS_TEST_ADMIN_USERNAME!,
          process.env.TRELLIS_TEST_ADMIN_PASSWORD!,
        ],
        [
          origins[0],
          "state-browser",
          "state-second",
          "separate state password",
        ],
      ]) {
        const isolated = await browser.newContext();
        const page = await isolated.newPage();
        await open(page, origin, clientId, user, password);
        assert.equal(await call(page, "get"), null);
        await call(page, "create", "isolated");
        assert.deepEqual(await call(first, "get"), winner);
        await isolated.close();
      }
      await call(second, "closeWatch");
      await browser.close();
      await context.close();
      const restarted = once(process.stdin, "data");
      process.stdout.write("STATE_RESTART\n");
      await restarted;
      process.stdin.pause();
      context = await chromium.launchPersistentContext(
        join(directory, "profile"),
        { headless: true },
      );
      first = await context.newPage();
      await open(
        first,
        origins[0],
        "state-browser",
        undefined,
        undefined,
        true,
      );
      assert.deepEqual(await call(first, "get"), winner);
      await call(first, "watch");
      assert.deepEqual(await call(first, "next"), winner);
      const deleted = call(first, "next");
      await call(first, "remove", winner.revision);
      assert.equal(await deleted, null);
      assert.equal(await call(first, "get"), null);
      await call(first, "closeWatch");
    } finally {
      await context.close();
      await browser.close();
      await Promise.all(
        servers.map(
          (server) =>
            new Promise<void>((resolve) => server.close(() => resolve())),
        ),
      );
      await rm(directory, { recursive: true, force: true });
    }
  },
);
