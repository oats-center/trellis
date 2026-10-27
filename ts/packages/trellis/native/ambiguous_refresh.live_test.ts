import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { request } from "node:https";
import { join } from "node:path";
import { TLSSocket } from "node:tls";
import { test } from "node:test";
import { createInterface } from "node:readline";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";

test(
  "a refused refresh keeps its token, while a real consumed refresh whose HTTP response is lost cannot be retried and old session references cannot release custody",
  { timeout: 45000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      let drop = false;
      let refuse = false;
      let tokens = 0;
      const targets = new WeakMap<object, string>();
      const forwarding = createServer((incoming, outgoing) => {
        const target = new URL(
          incoming.url ?? "/",
          `https://${targets.get(incoming.socket)}`,
        );
        if (target.pathname === "/token") tokens++;
        if (refuse && target.pathname === "/token") {
          // Answer as a rate-limiting reverse proxy would, before Trellis reads it.
          refuse = false;
          outgoing.writeHead(429, { "retry-after": "60" });
          outgoing.end();
          return;
        }
        const upstream = request(
          target,
          { method: incoming.method, headers: incoming.headers },
          (response) => {
            if (
              drop &&
              target.pathname === "/token" &&
              response.statusCode === 200
            ) {
              // The real engine already committed the refresh. Lose only its real HTTP response.
              response.resume();
              outgoing.destroy();
              return;
            }
            outgoing.writeHead(response.statusCode ?? 502, response.headers);
            response.pipe(outgoing);
          },
        );
        upstream.on("error", () => outgoing.destroy());
        incoming.pipe(upstream);
      });
      const proxy = createServer();
      proxy.on("request", (incoming, outgoing) =>
        forwarding.emit("request", incoming, outgoing),
      );
      // The loss happens inside the OAuth origin's TLS, so the proxy holds the
      // runtime's own certificate and re-encrypts to the real origin.
      const tls = join(runtime.workdir, "config", "trellis", "tls");
      const certificate = readFileSync(join(tls, "runtime.crt"));
      const privateKey = readFileSync(join(tls, "runtime.key"));
      proxy.on("connect", (incoming, socket, head) => {
        socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
        if (head.length) socket.unshift(head);
        const secure = new TLSSocket(socket, {
          isServer: true,
          cert: certificate,
          key: privateKey,
        });
        secure.on("error", () => secure.destroy());
        targets.set(secure, incoming.url ?? "");
        forwarding.emit("connection", secure);
      });
      proxy.listen(0, "127.0.0.1");
      await once(proxy, "listening");
      const address = proxy.address();
      assert.ok(address && typeof address === "object");
      const moduleUrl = (path: string) =>
        JSON.stringify(new URL(path, import.meta.url).href);
      const child = spawn(
        process.execPath,
        [
          "--experimental-transform-types",
          "--input-type=module",
          "-e",
          `
    import assert from "node:assert/strict";
    import { once } from "node:events";
    import { loginNativeTestClient } from ${moduleUrl("../../trellis-testkit/src/admin/native_auth.ts")};
    import { apps } from ${moduleUrl("../../trellis-testkit/trellis/index.js")};
    import { connectNativeSession, NativeOAuthClient, MemoryNativeOAuthCustody } from ${moduleUrl("../native.ts")};
    const custody = new MemoryNativeOAuthCustody();
    const login = await loginNativeTestClient({ issuer:process.env.TRELLIS_NATIVE_TEST_ISSUER, username:process.env.TRELLIS_NATIVE_TEST_USERNAME,password:process.env.TRELLIS_NATIVE_TEST_PASSWORD,custody,clientId:"trellis.cli",request:apps.cli.request });
    const session = login.session;
    const scope = session.scope; const trust = session.trust;
    const connected = await connectNativeSession({session,servers:login.servers}).orThrow();
    await session.close();
    const contender = await NativeOAuthClient.open({custody,scope:session.scope,trust:session.trust});
    assert.equal(contender.isErr(),true);
    const successful = (await session.refresh()).orThrow(); assert.equal(successful.auth,session.auth); assert.equal(successful.runtimeId,session.runtimeId); assert.equal(successful.principalId,session.principalId);
    console.log("busy"); await once(process.stdin,"data");
    const refused = await session.refresh(); assert.equal(refused.isErr(),true); assert.equal(refused.error.reason,"OAuthBusy");
    (await session.refresh()).orThrow();
    console.log("ready"); await once(process.stdin,"data");
    const refresh = await session.refresh(); assert.equal(refresh.isErr(),true); assert.equal(refresh.error.reason,"OAuthRecoveryRequired");
    console.log("spent"); await once(process.stdin,"data");
    const retry = await session.refresh(); assert.equal(retry.isErr(),true); assert.equal(retry.error.reason,"OAuthRecoveryRequired");
    await connected.close();
    const reopened = (await NativeOAuthClient.open({custody,scope,trust})).orThrow();
    const restart = await reopened.resume(); assert.equal(restart.isErr(),true); assert.equal(restart.error.reason,"OAuthRecoveryRequired"); await reopened.close();
    console.log("done");
  `,
        ],
        {
          env: {
            ...process.env,
            TRELLIS_NATIVE_TEST_ISSUER: runtime.oauthIssuer,
            TRELLIS_NATIVE_TEST_USERNAME: runtime.adminUsername,
            TRELLIS_NATIVE_TEST_PASSWORD: runtime.adminPassword,
            NODE_EXTRA_CA_CERTS: runtime.caCertificate,
            NODE_USE_ENV_PROXY: "1",
            HTTP_PROXY: `http://127.0.0.1:${address.port}`,
            HTTPS_PROXY: `http://127.0.0.1:${address.port}`,
            NO_PROXY: "",
          },
          stdio: ["pipe", "pipe", "inherit"],
        },
      );
      const exited = once(child, "exit");
      assert.ok(child.stdout && child.stdin);
      const lines = createInterface({ input: child.stdout });
      const iterator = lines[Symbol.asyncIterator]();
      const nextLine = async () => {
        const result = await Promise.race([
          iterator.next(),
          exited.then(() => {
            throw new Error(
              "native client exited before completing loss scenario",
            );
          }),
          new Promise<never>((_, reject) =>
            context.signal.addEventListener(
              "abort",
              () => reject(context.signal.reason),
              { once: true },
            ),
          ),
        ]);
        assert.equal(result.done, false);
        return result.value;
      };
      try {
        // A refused refresh leaves the token unspent; the next one succeeds with a
        // single token POST because discovery already supplied the DPoP nonce.
        assert.equal(await nextLine(), "busy");
        refuse = true;
        const beforeBusy = tokens;
        child.stdin.write("busy\n");
        assert.equal(await nextLine(), "ready");
        assert.equal(tokens, beforeBusy + 2);
        drop = true;
        child.stdin.write("refresh\n");
        assert.equal(await nextLine(), "spent");
        const attempts = tokens;
        assert.ok(attempts > 0);
        child.stdin.write("retry\n");
        assert.equal(await nextLine(), "done");
        child.stdin.end();
        assert.equal(
          tokens,
          attempts,
          "spent refresh must not reach the real engine a second time",
        );
        const [code] = await exited;
        assert.equal(code, 0);
      } finally {
        child.kill("SIGKILL");
        await exited;
        lines.close();
        forwarding.closeAllConnections();
        proxy.closeAllConnections();
        proxy.close();
        forwarding.close();
      }
    });
  },
);
