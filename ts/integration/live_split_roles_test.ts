import { TrellisService } from "@oatscenter/trellis/service";
import { assert } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { participants as webParticipants } from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary used for the platform and each split role. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../rust/target/debug/trellis-server", import.meta.url),
    );
}

async function freeTcpPort(): Promise<number> {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const port = (listener.addr as Deno.NetAddr).port;
  listener.close();
  return port;
}

async function startSplitRole(
  runtime: { workdir: string },
  role: "health" | "jobs" | "events",
): Promise<{
  status: Promise<Deno.CommandStatus>;
  kill: () => void;
}> {
  const configDir = join(runtime.workdir, "trellis");
  const platformConfig = await Deno.readTextFile(
    join(configDir, "config.toml"),
  );
  const port = await freeTcpPort();
  const configPath = join(configDir, `${role}-config.toml`);
  await Deno.writeTextFile(
    configPath,
    platformConfig.replace(/\nport = \d+\n/, `\nport = ${port}\n`),
  );
  const child = new Deno.Command(serverBinary(), {
    args: ["--config", configPath, role],
    env: {
      PATH: Deno.env.get("PATH") ?? "",
      HOME: Deno.env.get("HOME") ?? "",
      TRELLIS_CACHE: Deno.env.get("TRELLIS_CACHE") ?? "",
    },
    // Nobody reads these pipes; piping would eventually block a busy role.
    stdout: "null",
    stderr: "null",
  }).spawn();
  return {
    status: child.status,
    kill: () => child.kill("SIGTERM"),
  };
}

function jsonFrame(frame: unknown): Record<string, unknown> {
  if (frame instanceof Uint8Array) {
    return JSON.parse(new TextDecoder().decode(frame));
  }
  if (frame && typeof frame === "object") {
    return frame as Record<string, unknown>;
  }
  throw new Error(`unexpected watch frame: ${typeof frame}`);
}

const splitOptions = {
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "platform"],
    },
  },
};

Deno.test("BI03 split health role serves a real Health.Watch on its own owner", async () => {
  await withTrellisRuntime(async (runtime) => {
    const split = await startSplitRole(runtime, "health");
    try {
      const client = await runtime.connectClient({
        name: "bi03-split-health",
        contract: webParticipants.Console.participant,
      });
      const frame = await runtime.waitFor(async () => {
        try {
          const feed = await client.healthWatch({}).orThrow();
          const first = await feed[Symbol.asyncIterator]().next();
          await feed[Symbol.asyncIterator]().return?.();
          return first.done ? undefined : first.value;
        } catch {
          return undefined;
        }
      }, { timeoutMs: 60_000 });
      assert(frame !== undefined, "split health role never served a frame");
      await client.connection.close();
    } finally {
      split.kill();
      await split.status;
    }
  }, splitOptions);
});

Deno.test("BI03 split Jobs role emits queryInvalidated", async () => {
  await withTrellisRuntime(async (runtime) => {
    const health = await startSplitRole(runtime, "health");
    const events = await startSplitRole(runtime, "events");
    const split = await startSplitRole(runtime, "jobs");
    try {
      const key = await runtime.registerService({
        name: "bi03-split-jobs-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: "bi03-split-jobs-provider",
        seed: key.seed,
      }).orThrow();
      const client = await runtime.connectClient({
        name: "bi03-split-jobs",
        contract: webParticipants.Console.participant,
      });
      try {
        const abort = new AbortController();
        try {
          const feed = await client.jobsWatch({ includeInitial: false }, {
            signal: abort.signal,
          }).orThrow();
          const frames: Record<string, unknown>[] = [];
          const pump = (async () => {
            for await (const frame of feed) frames.push(jsonFrame(frame));
          })();
          await runtime.waitFor(() =>
            frames.some((frame) => frame.kind === "ready")
          );
          await service.jobs.work.create({ value: "split-watch-live" })
            .orThrow();
          await runtime.waitFor(
            () => frames.some((frame) => frame.kind === "queryInvalidated"),
            { timeoutMs: 30_000 },
          );
          abort.abort();
          await pump.catch(() => undefined);
        } finally {
          abort.abort();
        }
      } finally {
        await client.connection.close();
        await service.stop();
      }
    } finally {
      split.kill();
      await split.status;
      events.kill();
      await events.status;
      health.kill();
      await health.status;
    }
  }, splitOptions);
});

Deno.test("BI03 split Events role emits published events", async () => {
  await withTrellisRuntime(async (runtime) => {
    const health = await startSplitRole(runtime, "health");
    const split = await startSplitRole(runtime, "events");
    const jobs = await startSplitRole(runtime, "jobs");
    try {
      const key = await runtime.registerService({
        name: "bi03-split-events-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: "bi03-split-events-provider",
        seed: key.seed,
      }).orThrow();
      const client = await runtime.connectClient({
        name: "bi03-split-events",
        contract: webParticipants.Console.participant,
      });
      try {
        const abort = new AbortController();
        try {
          const feed = await client.eventsWatch({}, { signal: abort.signal })
            .orThrow();
          const frames: Record<string, unknown>[] = [];
          const pump = (async () => {
            for await (const frame of feed) frames.push(jsonFrame(frame));
          })();
          await runtime.waitFor(async () => {
            if (
              frames.some((frame) =>
                Array.isArray(frame.events) && frame.events.length > 0
              )
            ) {
              return true;
            }
            await service.publishChanged({ value: "split-watch-live" })
              .orThrow();
            return false;
          }, { timeoutMs: 30_000 });
          abort.abort();
          await pump.catch(() => undefined);
        } finally {
          abort.abort();
        }
      } finally {
        await client.connection.close();
        await service.stop();
      }
    } finally {
      jobs.kill();
      await jobs.status;
      split.kill();
      await split.status;
      health.kill();
      await health.status;
    }
  }, splitOptions);
});
