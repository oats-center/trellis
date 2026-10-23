import assert from "node:assert/strict";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";
import { ulid } from "ulid";

import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { trustInstallationCa } from "../../trellis-testkit/src/tls_trust.ts";
import { apps, types } from "../../trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../native.ts";

const exec = promisify(execFile);

test(
  "failed broker enforcement survives Auth restart and kicks only its exact attachment",
  { timeout: 150_000 },
  async (context) => {
    const directory = await mkdtemp(join(tmpdir(), "native-enforcement-"));
    // The bundle lives inside the temporary directory, so its generated CA
    // directory (a sibling of the bundle) is removed with it.
    const bundle = join(directory, "bundle");
    const cli = resolve(
      process.env.TRELLIS_TEST_CLI_BIN ?? "target/release/trellis",
    );
    const serverBinary = resolve(
      process.env.TRELLIS_TEST_SERVER_BIN ?? "target/release/trellis-server",
    );
    const natsBinary = process.env.TRELLIS_TEST_NATS_BIN ?? "nats-server";
    const ports: number[] = [];
    for (let index = 0; index < 5; index += 1) {
      const listener = createServer();
      listener.listen(0, "127.0.0.1");
      await once(listener, "listening");
      const address = listener.address();
      assert.ok(address && typeof address === "object");
      ports.push(address.port);
      await new Promise<void>((done) => listener.close(() => done()));
    }
    const [http, broker, monitor, websocket, oauth] = ports;
    const issuer = `https://127.0.0.1:${oauth}`;
    const children: ChildProcess[] = [];
    const connections: Array<() => Promise<unknown>> = [];
    let serverLog = "";
    let brokerLog = "";
    const waitFor = async (
      description: string,
      predicate: () => Promise<boolean>,
      milliseconds = 20_000,
    ) => {
      const deadline = Date.now() + milliseconds;
      while (Date.now() < deadline) {
        if (await predicate()) return;
        await delay(50, undefined, { signal: context.signal });
      }
      assert.fail(`${description}\n${serverLog}\n${brokerLog}`);
    };
    const stop = async (child: ChildProcess) => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      const exited = once(child, "exit");
      child.kill("SIGINT");
      await exited;
    };
    try {
      await exec(cli, [
        "init",
        "config",
        "--out",
        bundle,
        "--trellis-port",
        String(http),
        "--nats-port",
        String(broker),
        "--nats-monitor-port",
        String(monitor),
        "--nats-ws-port",
        String(websocket),
        "--oauth-origin",
        issuer,
        "--oauth-port",
        String(oauth),
        "--nats-data-dir",
        `${directory}/jetstream`,
      ]);
      // The installation serves its generated TLS; this test trusts its CA.
      trustInstallationCa(`${bundle}/tls/ca.crt`);
      const configPath = `${bundle}/config.toml`;
      const config =
        (await readFile(configPath, "utf8")) +
        `\n[paths]\n${["data", "state", "cache", "runtime", "logs"].map((name) => `${name} = ${JSON.stringify(`${directory}/${name}`)}`).join("\n")}\n`;
      // The broker accepts this operational credential, but it has no access to
      // the system-account connection-management responders. This is an ordinary
      // broker permission failure, not a substituted broker or runtime hook.
      await writeFile(
        configPath,
        config.replace(
          'system_creds_path = "./nats/creds/system.creds"',
          'system_creds_path = "./nats/creds/trellis-auth.creds"',
        ),
      );
      const natsConfig = `${bundle}/nats/nats.conf`;
      const username = "enforcement-admin";
      const password = "enforcement-fixture-password";
      await exec(serverBinary, [
        "bootstrap-admin",
        "--config",
        configPath,
        "--username",
        username,
        "--password",
        password,
      ]);
      const nats = spawn(natsBinary, ["-c", natsConfig]);
      nats.stdout.on("data", (chunk) => {
        brokerLog += chunk;
      });
      nats.stderr.on("data", (chunk) => {
        brokerLog += chunk;
      });
      children.push(nats);
      await waitFor("broker readiness", async () =>
        fetch(`http://127.0.0.1:${monitor}/healthz`).then(
          (r) => r.ok,
          () => false,
        ),
      );
      let server = spawn(serverBinary, ["all", "--config", configPath]);
      children.push(server);
      server.stdout.on("data", (chunk) => {
        serverLog += chunk;
      });
      server.stderr.on("data", (chunk) => {
        serverLog += chunk;
      });
      await waitFor("runtime readiness", async () =>
        fetch(`https://localhost:${http}/readyz`).then(
          (r) => r.ok,
          () => false,
        ),
      );
      const request = apps.cli.request;
      const adminLogin = await loginNativeTestClient({
        issuer,
        username,
        password,
        clientId: "trellis.cli",
        request,
        administrativeCapabilities: ["trellis.managePrincipals"],
        signal: context.signal,
      });
      const admin = await connectNativeClient({
        ...adminLogin,
        request,
        signal: context.signal,
      }).orThrow();
      connections.push(() => admin.connection.close());
      // Two independent sessions of the same principal isolate physical target
      // selection from principal identity. Both stay on the same live broker.
      const siblingLogin = await loginNativeTestClient({
        issuer,
        username,
        password,
        clientId: "trellis.cli",
        request,
        signal: context.signal,
      });
      const sibling = await connectNativeClient({
        ...siblingLogin,
        request,
        signal: context.signal,
      }).orThrow();
      connections.push(() => sibling.connection.close());
      const authority = admin.ownAuthority.current;
      const siblingAuthority = sibling.ownAuthority.current;
      assert.ok(authority && siblingAuthority);
      const { principalId } = types.AuthPrincipalRequestCodec.decode({
        principalId: authority.principalId,
      });
      const before = await admin
        .connectionsList({ principalId, state: "admitted" })
        .orThrow();
      assert.equal(before.items.length, 2);
      const target = before.items.find(
        (attachment) =>
          attachment.authorizationSessionId ===
          siblingAuthority.authorizationSessionId,
      );
      assert.ok(target);
      const untouched = before.items.find(
        (attachment) => attachment.attachmentId !== target.attachmentId,
      );
      assert.ok(untouched);
      const kicked = await admin
        .connectionsKick({
          attachmentId: target.attachmentId,
          idempotencyKey: ulid(),
        })
        .orThrow();
      assert.equal(kicked.enforcement.pendingKicks, 1n);
      assert.notEqual(kicked.enforcement.state, "completed");
      await waitFor(
        "real broker request failure leaves durable work pending",
        async () => serverLog.includes("Auth effect remains pending"),
      );
      const stillAttached = await admin
        .connectionsList({ principalId, state: "admitted" })
        .orThrow();
      assert.ok(
        stillAttached.items.some(
          (item) => item.attachmentId === target.attachmentId,
        ),
      );
      await sibling.sessionsList({}).orThrow();
      await stop(server);
      await writeFile(configPath, config);
      server = spawn(serverBinary, ["all", "--config", configPath]);
      children.push(server);
      server.stdout.on("data", (chunk) => {
        serverLog += chunk;
      });
      server.stderr.on("data", (chunk) => {
        serverLog += chunk;
      });
      await waitFor(
        "restarted runtime readiness",
        async () =>
          fetch(`https://localhost:${http}/readyz`).then(
            (r) => r.ok,
            () => false,
          ),
        45_000,
      );
      await waitFor(
        "restarted Auth drains the persisted exact kick",
        async () => {
          const current = await admin
            .connectionsList({ principalId })
            .orThrow();
          return current.items.some(
            (item) =>
              item.attachmentId === target.attachmentId &&
              item.state === "closed",
          );
        },
      );
      const after = await admin.connectionsList({ principalId }).orThrow();
      assert.equal(
        after.items.find((item) => item.attachmentId === untouched.attachmentId)
          ?.state,
        "admitted",
      );
      const inventory = await fetch(
        `http://127.0.0.1:${monitor}/connz?auth=1`,
      ).then((response) => response.json());
      assert.equal(
        inventory.connections.some(
          (connection: { cid: number }) =>
            BigInt(connection.cid) === target.brokerClientId,
        ),
        false,
      );
      assert.equal(
        inventory.connections.some(
          (connection: { cid: number }) =>
            BigInt(connection.cid) === untouched.brokerClientId,
        ),
        true,
      );
      await admin.sessionsList({}).orThrow();
    } finally {
      for (const close of connections.reverse()) await close();
      for (const child of children.reverse()) await stop(child);
      await rm(directory, { recursive: true, force: true });
    }
  },
);
