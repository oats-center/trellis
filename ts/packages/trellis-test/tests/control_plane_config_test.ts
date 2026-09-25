import { assertEquals, assertNotEquals, assertThrows } from "@std/assert";
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

Deno.test("reserveLocalPort lets the OS choose distinct ports", () => {
  const first = reserveLocalPort();
  const second = reserveLocalPort();
  try {
    assertNotEquals(first.port, second.port);
    assertThrows(
      () => reserveLocalPort(first.port),
      Deno.errors.AddrInUse,
    );
    first.releaseForSpawn();
    reserveLocalPort(first.port).release();
  } finally {
    first.release();
    second.release();
  }
});

Deno.test("writeTrellisConfig stages credentials without embedding their values", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "trellis-config-test-" });
  try {
    const natsDir = join(workdir, "nats");
    const seeds = {
      "auth-issuer-signing.seed": "issuer-seed\n",
      "auth-target-signing.seed": "target-seed\n",
      "auth-sx.seed": "sx-seed\n",
    };
    await Deno.mkdir(join(natsDir, "secrets"), { recursive: true });
    for (const [name, seed] of Object.entries(seeds)) {
      await Deno.writeTextFile(join(natsDir, "secrets", name), seed);
    }

    const config = buildControlPlaneConfig({
      workdir,
      natsUrl: "nats://127.0.0.1:4222",
      websocketUrl: "ws://127.0.0.1:8080",
      manifest: testManifest(),
      port: 3000,
    });
    const configPath = await writeTrellisConfig({ workdir, config });
    const text = await Deno.readTextFile(configPath);

    for (const [name, seed] of Object.entries(seeds)) {
      assertEquals(
        await Deno.readTextFile(join(workdir, "trellis", name)),
        seed,
      );
      assertEquals(
        await Deno.readTextFile(join(natsDir, "secrets", name)),
        seed,
      );
      assertEquals(text.includes(seed.trim()), false);
    }
  } finally {
    await Deno.remove(workdir, { recursive: true });
  }
});
