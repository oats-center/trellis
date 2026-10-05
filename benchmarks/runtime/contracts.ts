import { parseArgs } from "@std/cli/parse-args";
import { fromFileUrl, relative, resolve, toFileUrl } from "@std/path";
import { z } from "zod";

const root = fromFileUrl(new URL("../../", import.meta.url));
const args = parseArgs(Deno.args, {
  string: ["cli", "output", "actions"],
  default: { actions: "1,32,128" },
});
const cli = resolve(z.string().parse(args.cli));
const output = resolve(z.string().parse(args.output));
const counts = z.array(z.coerce.number().int().positive().max(1024)).nonempty()
  .parse(args.actions.split(","));
for (const count of counts) {
  const dir = `${output}/contracts-${count}`;
  await Deno.mkdir(dir, { recursive: true });
  const actions = Array.from({ length: count }, (_, index) => `Echo${index}`);
  await Deno.writeTextFile(
    `${dir}/contract.trellis`,
    `import { auth } from trellis;
model Value { value: string; }
api scale@v1 {
  title "Contract scale";
  description "Measure authorization and connection cost with a growing complete RPC surface.";
  ${
      actions.map((name) => `rpc ${name} { input Value; output Value; }`).join(
        "\n  ",
      )
    }
  capabilities { public { allows { ${
      actions.map((name) => `rpc ${name};`).join(" ")
    } } } }
}
service Provider { implements scale; }
app Caller {
  use scale { ${actions.map((name) => `rpc ${name};`).join(" ")} }
  use auth { rpc Sessions.Me; rpc Sessions.Logout; }
}
`,
  );
  await Deno.writeTextFile(
    `${dir}/trellis.toml`,
    `[package]
name = "scaleperf${count}"
version = "0.0.0"
[sources]
scale = "contract.trellis"
[generate.typescript]
output = "packages/scale"
[dependencies]
trellis = { package = "trellis", path = ${
      JSON.stringify(relative(dir, `${root}/crates/runtime`))
    } }
`,
  );
  const started = performance.now();
  const updated = await new Deno.Command(cli, { args: ["update"], cwd: dir })
    .output();
  if (!updated.success) {
    throw new Error(new TextDecoder().decode(updated.stderr));
  }
  const generated = await new Deno.Command(cli, {
    args: ["generate"],
    cwd: dir,
  }).output();
  await Deno.writeTextFile(
    `${dir}/generation.log`,
    new TextDecoder().decode(generated.stdout) +
      new TextDecoder().decode(generated.stderr),
  );
  if (!generated.success) {
    throw new Error(`Contract generation failed; ${dir}/generation.log`);
  }
  await Deno.writeTextFile(
    `${dir}/generation.json`,
    JSON.stringify({
      actions: count,
      generationMs: performance.now() - started,
    }),
  );
  // Statically import the actual generated package: public SDK types remain honest.
  await Deno.writeTextFile(
    `${dir}/worker.ts`,
    `
import { TrellisClient, Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./packages/scale/index.js";
import { completeLocalAuthFlow } from ${
      JSON.stringify(
        toFileUrl(`${root}/ts/packages/trellis-testkit/src/admin/auth_flow.ts`)
          .href,
      )
    };
import { WorkerOptions, summarize, type Sample } from ${
      JSON.stringify(toFileUrl(`${root}/benchmarks/runtime/model.ts`).href)
    };
const options = WorkerOptions.parse(JSON.parse(await new Response(Deno.stdin.readable).text()));
if (options.role === "provider") {
  if (!options.serviceSeed) throw new Error("Service seed missing");
  const service = await TrellisService.connect({ trellisUrl: options.trellisUrl, participant: participants.Provider.participant, seed: options.serviceSeed, name: "contract-scale-provider" }).orThrow();
  ${
      actions.map((name) =>
        `await service.handle${name}(({ input }) => Result.ok(input));`
      ).join("\n  ")
    }
  await Deno.writeTextFile(options.output + "/provider-" + options.providerIndex + ".json", JSON.stringify({ ready: true }));
  await service.wait();
} else {
  const samples: Sample[] = [];
  for (let index = 0; index < options.samples; index++) {
    let sessionId: string | undefined;
    for (const resume of [false, true]) {
      const started = performance.now();
      const row: Sample = { scenario: "contract/${count}-actions/" + (resume ? "resume-first-rpc" : "fresh-login-first-rpc"), transport: "trellis", startedUnixMs: Date.now(), durationMs: 0 };
      await Deno.writeTextFile(options.output + "/phase.txt", row.scenario);
      let client;
      try {
        client = await TrellisClient.connect({ trellisUrl: options.trellisUrl, participant: participants.Caller.participant, name: "contract-scale-caller", auth: { mode: "session_key", sessionKeySeed: options.seeds[index], sessionId: resume ? sessionId : undefined, redirectTo: options.trellisUrl + "/_trellis/test/client-auth" }, onAuthRequired: (context) => completeLocalAuthFlow({ trellisUrl: options.trellisUrl, loginUrl: context.loginUrl, password: options.password }) }).orThrow();
        const connected = performance.now();
        const value = "scaled-" + index;
        if ((await client.echo0({ value }).orThrow()).value !== value) throw new Error("Incorrect generated response");
        row.durationMs = performance.now() - started;
        row.connectMs = connected - started;
        row.firstRpcMs = performance.now() - connected;
        sessionId = (await client.sessionsMe({}).orThrow()).session?.sessionId;
        if (!sessionId) throw new Error("Contract-scale session missing");
        if (resume) await client.sessionsLogout({}).orThrow();
      } catch (error) { row.durationMs = performance.now() - started; row.error = String(error); }
      finally { if (client) await client.connection.close(); }
      samples.push(row);
      await Deno.writeTextFile(options.output + "/samples.json", JSON.stringify({ samples, summary: summarize(samples), windows: [] }, null, 2));
    }
  }
  if (samples.some((row) => row.error)) throw new Error("Contract-scale workload failed; samples retained");
}
`,
  );
  const check = await new Deno.Command(Deno.execPath(), {
    args: ["check", "-c", `${root}/ts/deno.json`, `${dir}/worker.ts`],
  }).output();
  if (!check.success) throw new Error(new TextDecoder().decode(check.stderr));
}
