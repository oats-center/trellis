import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { z } from "zod";

test(
  "packed SDK root and native entrypoints work in an empty npm consumer",
  { timeout: 120000 },
  async () => {
    const directory = await mkdtemp(join(tmpdir(), "native-npm-consumer-"));
    const run = (command: string, args: string[], cwd: string) => {
      const result = spawnSync(command, args, {
        cwd,
        encoding: "utf8",
        timeout: 90000,
      });
      assert.equal(
        result.status,
        0,
        `${command} ${args.join(" ")}\n${result.stderr}\n${result.stdout}`,
      );
      return result.stdout;
    };
    try {
      const tarballs: string[] = [];
      for (const path of ["../../result/npm/", "../npm/"]) {
        const packed = run(
          "npm",
          ["pack", "--json", "--pack-destination", directory],
          fileURLToPath(new URL(path, import.meta.url)),
        );
        const files = z
          .array(z.object({ filename: z.string() }))
          .parse(JSON.parse(packed));
        tarballs.push(...files.map((file) => join(directory, file.filename)));
      }
      run(
        "npm",
        [
          "install",
          "--prefix",
          directory,
          "--ignore-scripts",
          "--no-audit",
          "--no-fund",
          ...tarballs,
        ],
        directory,
      );
      run(
        process.execPath,
        [
          "--input-type=module",
          "-e",
          `
      import assert from "node:assert/strict";
      const root = await import("@oatscenter/trellis");
      const native = await import("@oatscenter/trellis/native");
      const seed = crypto.getRandomValues(new Uint8Array(32));
      const auth = await root.createAuth({sessionKeySeed:Buffer.from(seed).toString("base64url")});
      const message = new TextEncoder().encode("installed npm consumer");
      const signature = await auth.sign(message);
      const publicKey = await crypto.subtle.importKey("raw",Buffer.from(auth.sessionKey,"base64url"),{name:"Ed25519"},false,["verify"]);
      assert.equal(await crypto.subtle.verify({name:"Ed25519"},publicKey,signature,message),true);
      const rejected = await native.discoverNativeOAuth("http://example.com");
      assert.equal(rejected.isErr(),true);
      assert.equal(rejected.error.reason,"InvalidIssuer");
    `,
        ],
        directory,
      );
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);
