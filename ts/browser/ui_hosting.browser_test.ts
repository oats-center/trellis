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
  const vite = startVite(fromFileUrl(new URL("../../web/", import.meta.url)));
  try {
    await waitForUrl("http://127.0.0.1:5173/login", vite);
    const runtime = await startTrellisRuntime({
      webSource: { proxy: "http://127.0.0.1:5173" },
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
        const requests: string[] = [];
        const sockets: string[] = [];
        page.on("pageerror", (error) => console.error(error));
        page.on("console", (message) => {
          if (message.type() === "error") console.error(message.text());
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
      } finally {
        await browser.close();
      }
    } finally {
      await runtime.stop();
    }
  } finally {
    try {
      vite.kill("SIGTERM");
    } catch (error) {
      if (
        !(error instanceof Deno.errors.NotFound) &&
        !(error instanceof TypeError &&
          error.message === "Child process has already terminated")
      ) throw error;
    }
    await vite.status;
  }
});

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
    stdout: "inherit",
    stderr: "inherit",
  }).spawn();
}

async function waitForUrl(
  url: string,
  child: Deno.ChildProcess,
): Promise<void> {
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
        `child: ${
          exit === undefined
            ? "still running"
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
