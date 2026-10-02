import { assert, assertEquals, assertStringIncludes } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { chromium } from "playwright";

import { startTrellisRuntime } from "../integration/_support/runtime.ts";

Deno.test("embedded web source serves both applications", async () => {
  const runtime = await startTrellisRuntime({
    trellis: {
      command: {
        cmd: prebuiltServer(),
        args: ["--config", "{config}", "all"],
      },
    },
  });
  try {
    const browser = await chromium.launch({ headless: true });
    try {
      const page = await browser.newPage();
      for (const path of ["/login", "/console/login"]) {
        const response = await page.goto(`${runtime.trellisUrl}${path}`, {
          waitUntil: "domcontentloaded",
        });
        assertEquals(response?.status(), 200, path);
        assertEquals(new URL(page.url()).origin, runtime.trellisUrl);
        assertEquals(
          await page.evaluate(() =>
            (globalThis as typeof globalThis & {
              __TRELLIS_RUNTIME_CONFIG__?: { authUrl?: string };
            }).__TRELLIS_RUNTIME_CONFIG__?.authUrl
          ),
          runtime.trellisUrl,
        );
      }
    } finally {
      await browser.close();
    }
  } finally {
    await runtime.stop();
  }
});

Deno.test("configured UI directories serve both applications", async () => {
  const runtime = await startTrellisRuntime();
  try {
    const root = join(runtime.workdir, "trellis");
    await Deno.mkdir(join(root, "test-portal/assets/login"), {
      recursive: true,
    });
    await Deno.mkdir(join(root, "test-console/assets"), { recursive: true });
    await Deno.writeTextFile(
      join(root, "test-portal/200.html"),
      "<h1>portal directory marker</h1>",
    );
    await Deno.writeTextFile(
      join(root, "test-portal/assets/login/probe.txt"),
      "portal asset",
    );
    await Deno.writeTextFile(
      join(root, "test-console/index.html"),
      "<h1>console directory marker</h1>",
    );
    await Deno.writeTextFile(
      join(root, "test-console/assets/probe.txt"),
      "console asset",
    );

    const configPath = join(root, "config.toml");
    const config = await Deno.readTextFile(configPath);
    assertStringIncludes(config, "[http]\n");
    await Deno.writeTextFile(
      configPath,
      config.replace(
        "[http]\n",
        '[http]\nportal_source = { directory = "./test-portal" }\nconsole_source = { directory = "./test-console" }\n',
      ),
    );
    await runtime.restartControlPlane();

    for (
      const [path, body, contentType] of [
        ["/login", "portal directory marker", "text/html"],
        ["/login/deep/route", "portal directory marker", "text/html"],
        ["/assets/login/probe.txt", "portal asset", "text/plain"],
        ["/console", "console directory marker", "text/html"],
        ["/console/deep/route", "console directory marker", "text/html"],
        ["/console/assets/probe.txt", "console asset", "text/plain"],
      ] as const
    ) {
      const response = await fetch(`${runtime.trellisUrl}${path}`);
      assertEquals(response.status, 200, path);
      assertStringIncludes(await response.text(), body);
      assertStringIncludes(
        response.headers.get("content-type") ?? "",
        contentType,
      );
    }
  } finally {
    await runtime.stop();
  }
});

Deno.test("the unified web source is fully reverse proxied through Trellis", async () => {
  const upstream = "http://127.0.0.1:5173";
  const spawnStartedAt = new Date().toISOString();
  const vite = startVite(fromFileUrl(new URL("../../web/", import.meta.url)));
  const lifecycle = [
    `PID ${vite.pid}: spawn started ${spawnStartedAt}; spawned ${
      new Date().toISOString()
    }`,
  ];
  const viteStatus = vite.status.then((status) => {
    lifecycle.push(
      `PID ${vite.pid}: exit observed ${
        new Date().toISOString()
      }; code=${status.code}, signal=${status.signal}`,
    );
    return status;
  });
  const viteOutput = { stdout: "", stderr: "" };
  const processDrained = Promise.allSettled([
    viteStatus,
    ...(["stdout", "stderr"] as const).map(async (name) => {
      for await (
        const chunk of vite[name].pipeThrough(new TextDecoderStream())
      ) {
        viteOutput[name] += chunk;
      }
    }),
  ]);
  let runtimeOutput = "";
  let failure: { cause: unknown } | undefined;
  const cleanupErrors: unknown[] = [];
  try {
    await waitForUrl(`${upstream}/login`, vite);
    const runtime = await startTrellisRuntime({
      webSource: { proxy: upstream },
      trellis: {
        command: {
          cmd: prebuiltServer(),
          args: ["--config", "{config}", "all"],
        },
      },
    });
    try {
      const browser = await chromium.launch({ headless: true });
      const diagnostics: string[] = [];
      const failedRequests: string[] = [];
      const captures: Promise<void>[] = [];
      const failedAssets = new Set<string>();
      try {
        const page = await browser.newPage();
        const requests: string[] = [];
        const sockets: string[] = [];
        page.on("pageerror", (error) => console.error(error));
        page.on("console", (message) => {
          if (message.type() === "error") console.error(message.text());
        });
        page.on("requestfailed", (request) => {
          const at = new Date().toISOString();
          const error = request.failure()?.errorText;
          if (failedRequests.length < 20) {
            failedRequests.push(
              `${at} ${request.url()} ${error}`.slice(0, 2048),
            );
          }
          const url = new URL(request.url());
          const resourceType = request.resourceType();
          if (
            url.origin === runtime.trellisUrl &&
            (resourceType === "script" || resourceType === "stylesheet") &&
            failedAssets.size < 8
          ) {
            failedAssets.add(url.href);
          }
        });
        page.on("response", (response) => {
          if (response.status() < 400 || captures.length >= 8) return;
          const url = new URL(response.url());
          const resourceType = response.request().resourceType();
          if (
            url.origin !== runtime.trellisUrl ||
            (resourceType !== "script" && resourceType !== "stylesheet")
          ) {
            return;
          }
          if (failedAssets.size < 8) failedAssets.add(url.href);
          const at = new Date().toISOString();
          const type = response.headers()["content-type"];
          const prefix = `${at} proxied ${url} ${response.status()} ${type}`;
          captures.push(
            response.body().then((body) => {
              const text = new TextDecoder().decode(body.subarray(0, 4096));
              diagnostics.push(`${prefix}\n${text}`);
            }).catch((error) => {
              diagnostics.push(prefix + "\n" + String(error).slice(0, 1024));
            }),
          );
        });
        page.on("request", (request) => requests.push(request.url()));
        page.on("websocket", (socket) => sockets.push(socket.url()));

        await page.goto(`${runtime.trellisUrl}/login`);
        await page.waitForLoadState("networkidle");
        assertStringIncludes(
          await page.title(),
          "Trellis",
          JSON.stringify(requests, null, 2),
        );
        assertEquals(new URL(page.url()).origin, runtime.trellisUrl);

        await page.goto(`${runtime.trellisUrl}/console/admin`);
        await page.locator("main").waitFor();
        assertEquals(new URL(page.url()).origin, runtime.trellisUrl);

        const authRequest = await page.evaluate(async () => {
          const response = await fetch("/auth/requests", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: "{}",
          });
          return {
            contentType: response.headers.get("content-type"),
            status: response.status,
          };
        });
        assertEquals(authRequest.status, 400);
        assertStringIncludes(authRequest.contentType ?? "", "application/json");
        assert(requests.some((url) => url.includes("/@fs/")));
        assert(requests.some((url) => url.includes("/@vite/")));
        assert(
          requests.every((url) => new URL(url).origin === runtime.trellisUrl),
          JSON.stringify(requests, null, 2),
        );
        const trellisHost = new URL(runtime.trellisUrl).host;
        assert(
          sockets.some((url) => new URL(url).host === trellisHost),
          JSON.stringify(sockets, null, 2),
        );
        assert(
          !requests.some((url) => new URL(url).host === "127.0.0.1:5173"),
        );
        assert(
          !sockets.some((url) => new URL(url).host === "127.0.0.1:5173"),
        );
      } catch (cause) {
        await Promise.all(captures);
        for (const asset of failedAssets) {
          const assetUrl = new URL(asset);
          const url = new URL(upstream);
          url.pathname = assetUrl.pathname;
          url.search = assetUrl.search;
          const at = new Date().toISOString();
          try {
            const response = await fetch(url, {
              redirect: "manual",
              signal: AbortSignal.timeout(5000),
            });
            const type = response.headers.get("content-type");
            const body = new Uint8Array(await response.arrayBuffer());
            const text = new TextDecoder().decode(body.subarray(0, 4096));
            diagnostics.push(
              `${at} direct ${url} ${response.status} ${type}\n${text}`,
            );
          } catch (error) {
            diagnostics.push(
              at + " direct " + url + " error: " + String(error).slice(0, 1024),
            );
          }
        }
        console.error([...failedRequests, ...diagnostics].join("\n"));
        throw cause;
      } finally {
        await Promise.all(captures);
        await browser.close();
      }
    } catch (cause) {
      runtimeOutput = runtime.controlPlaneOutput();
      throw cause;
    } finally {
      await runtime.stop();
    }
  } catch (cause) {
    failure = { cause };
  } finally {
    lifecycle.push(`PID ${vite.pid}: shutdown ${new Date().toISOString()}`);
    try {
      terminateVite(vite);
    } catch (cause) {
      cleanupErrors.push(cause);
    }
    for (const result of await processDrained) {
      if (result.status === "rejected") cleanupErrors.push(result.reason);
    }
    lifecycle.push(
      `PID ${vite.pid}: cleanup completed ${new Date().toISOString()}`,
    );
  }
  if (failure !== undefined) {
    const { cause } = failure;
    throw new Error(
      `${cause instanceof Error ? cause.message : String(cause)}\n` +
        `Vite lifecycle:\n${lifecycle.join("\n")}\n` +
        `Vite stdout:\n${viteOutput.stdout}\n` +
        `Vite stderr:\n${viteOutput.stderr}\n` +
        `Trellis runtime:\n${runtimeOutput}` +
        (cleanupErrors.length
          ? `\nVite cleanup errors:\n${cleanupErrors.join("\n")}`
          : ""),
      { cause },
    );
  }
  if (cleanupErrors.length) {
    throw new AggregateError(cleanupErrors, "Vite cleanup failed");
  }
});

function terminateVite(vite: Deno.ChildProcess): void {
  try {
    vite.kill("SIGTERM");
  } catch (cause) {
    if (
      !(cause instanceof Deno.errors.NotFound) &&
      !(cause instanceof TypeError &&
        cause.message === "Child process has already terminated")
    ) throw cause;
  }
}

function prebuiltServer(): string {
  const server = Deno.env.get("TRELLIS_TEST_SERVER_BIN");
  if (server === undefined) {
    throw new Error(
      "TRELLIS_TEST_SERVER_BIN must point to a prebuilt trellis-server",
    );
  }
  return server;
}

function startVite(
  cwd: string,
  env?: Record<string, string>,
): Deno.ChildProcess {
  return new Deno.Command(Deno.execPath(), {
    args: ["run", "-A", "vite", "dev"],
    cwd,
    env,
    stdout: "piped",
    stderr: "piped",
  }).spawn();
}

async function waitForUrl(
  url: string,
  child: Deno.ChildProcess,
): Promise<void> {
  const startedAt = new Date().toISOString();
  const deadline = Date.now() + 30_000;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 30_000);
  let exit: Deno.CommandStatus | undefined;
  void child.status.then((status) => {
    exit = status;
    controller.abort();
  });
  let lastStatus: number | undefined;
  let lastError: string | undefined;
  try {
    while (!controller.signal.aborted && Date.now() < deadline) {
      try {
        const response = await fetch(url, { signal: controller.signal });
        lastStatus = response.status;
        await response.body?.cancel();
        if (response.ok && !controller.signal.aborted) {
          console.log(`Vite ready: ${url} HTTP ${response.status}`);
          return;
        }
      } catch (error) {
        lastError = String(error);
      }
      const remaining = deadline - Date.now();
      if (controller.signal.aborted || remaining <= 0) break;
      await new Promise((resolve) =>
        setTimeout(resolve, Math.min(100, remaining))
      );
    }
    throw new Error(
      `${
        exit === undefined ? "Timed out waiting" : "Vite exited while waiting"
      } for ${url}; ` +
        `PID ${child.pid}; readiness started ${startedAt}; failed ${
          new Date().toISOString()
        }; child: ${
          exit === undefined
            ? "exit pending (not observed)"
            : `code=${exit.code}, signal=${exit.signal}`
        }; ` +
        `last HTTP status: ${lastStatus ?? "none"}; last fetch error: ${
          lastError ?? "none"
        }`,
    );
  } finally {
    clearTimeout(timer);
  }
}
