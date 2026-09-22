import { assertEquals, assertStringIncludes, assertThrows } from "@std/assert";
import { join } from "@std/path";
import {
  buildControlPlaneConfig,
  reserveLocalPort,
  writeTrellisConfig,
} from "../src/control_plane_config.ts";
import type { LocalNatsBootstrapManifest } from "../src/nats_bootstrap.ts";

function testManifest(): LocalNatsBootstrapManifest {
  return {
    accounts: {
      system: { name: "SYS", publicKey: "SYS_PUBLIC" },
      auth: { name: "AUTH", publicKey: "AUTH_PUBLIC" },
      trellis: { name: "TRELLIS", publicKey: "TRELLIS_PUBLIC" },
    },
    users: {
      system: { name: "system", publicKey: "SYSTEM_USER_PUBLIC" },
      authService: { name: "auth", publicKey: "AUTH_USER_PUBLIC" },
      trellisService: { name: "auth", publicKey: "TRELLIS_USER_PUBLIC" },
    },
    paths: {
      natsConfig: "nats.conf",
      jwtConfig: "jwt.conf",
      creds: {
        systemService: "creds/system.creds",
        authService: "creds/auth-auth.creds",
        trellisService: "creds/trellis-auth.creds",
      },
      secrets: {
        authIssuerSigning: "secrets/auth-issuer-signing.seed",
        authTargetSigning: "secrets/auth-target-signing.seed",
        authCalloutXKey: "secrets/auth-sx.seed",
      },
    },
  };
}

Deno.test("reserveLocalPort records a process-wide host lease", async () => {
  const lease = reserveLocalPort();
  const port = lease.port;
  const lockRoot = Deno.env.get("TRELLIS_TEST_PORT_LOCK_DIR") ??
    (Deno.build.os === "windows" ? Deno.env.get("TEMP") : "/tmp");
  if (lockRoot === undefined) {
    throw new Error("no temporary directory is configured");
  }
  const lockPath = `${lockRoot}/trellis-test-port-${port}.lock`;

  try {
    assertEquals((await Deno.readTextFile(lockPath)).trim(), String(Deno.pid));
    assertThrows(
      () => Deno.listen({ hostname: "127.0.0.1", port }),
      Deno.errors.AddrInUse,
    );
    lease.releaseForSpawn();
    assertThrows(
      () => reserveLocalPort(port),
      Error,
      "reserved by another test",
    );
    const listener = Deno.listen({ hostname: "127.0.0.1", port });
    listener.close();
  } finally {
    lease.release();
  }
});

Deno.test("writeTrellisConfig writes file-backed test control-plane config", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "trellis-config-test-" });
  try {
    const natsDir = join(workdir, "nats");
    await Deno.mkdir(join(natsDir, "secrets"), { recursive: true });
    await Deno.writeTextFile(
      join(natsDir, "secrets", "auth-issuer-signing.seed"),
      "issuer-seed\n",
    );
    await Deno.writeTextFile(
      join(natsDir, "secrets", "auth-target-signing.seed"),
      "target-seed\n",
    );
    await Deno.writeTextFile(
      join(natsDir, "secrets", "auth-sx.seed"),
      "sx-seed\n",
    );

    const config = buildControlPlaneConfig({
      workdir,
      natsUrl: "nats://127.0.0.1:4222",
      websocketUrl: "ws://127.0.0.1:8080",
      manifest: testManifest(),
      port: 3000,
      oauthProviders: {
        oidc_test: {
          type: "oidc",
          issuer: "https://idp.example",
          clientId: "test-client",
          clientSecret: "test-secret",
          logout: {
            enabled: true,
            endpoint: "https://idp.example/logout",
          },
        },
      },
    });
    const configPath = await writeTrellisConfig({ workdir, config });
    const text = await Deno.readTextFile(configPath);

    assertEquals(configPath, join(workdir, "trellis", "config.toml"));
    assertEquals(config.logLevel, "info");
    for (const section of ["platform", "jobs", "health", "events"]) {
      assertStringIncludes(
        text,
        `path = "${join(workdir, "trellis", `trellis.sqlite.${section}`)}"`,
      );
    }
    assertStringIncludes(
      text,
      `system_creds_path = "${join(workdir, "nats", "creds/system.creds")}"`,
    );
    assertStringIncludes(text, `[oauth.providers."oidc_test"]`);
    assertEquals(
      await Deno.readTextFile(
        join(workdir, "trellis", "auth-issuer-signing.seed"),
      ),
      "issuer-seed\n",
    );
  } finally {
    await Deno.remove(workdir, { recursive: true }).catch(() => undefined);
  }
});
