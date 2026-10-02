import { assert, assertEquals } from "@std/assert";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

/**
 * A Rust service keeps its required resource working while an optional resource
 * approved after connect becomes usable on the same logical handle, with no
 * explicit transport refresh: the grown call acquires a suitable generation on
 * its own deadline.
 */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 90,
    refreshLeadSeconds: 20,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 60,
  },
};

Deno.test(
  "a Rust service adopts a resource approved after connect without an explicit refresh",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const contract = participants.Provider.participant;
      await runtime.contracts.install({ contract });
      const requested = await runtime.contracts.requestApply({ contract });
      assertEquals(requested.status, "approval_required");
      if (requested.status !== "approval_required") return;
      await runtime.contracts.approveApply(requested.pendingId, {
        excludeResources: ["extras"],
      });
      const instance = await runtime.services.createInstance({
        name: "rust-resource-growth",
        contract,
      });

      const argv = rustFixtureArgv("resources");
      const child = new Deno.Command(argv[0], {
        args: argv.slice(1),
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_IDENTITY_SEED: instance.seed,
          TRELLIS_RESOURCE_GROWTH: "1",
        },
        stdin: "piped",
        stdout: "piped",
        stderr: "inherit",
      }).spawn();
      const reader = child.stdout.pipeThrough(new TextDecoderStream())
        .getReader();
      let output = "";
      const readUntil = async (marker: string, timeoutMs = 90_000) => {
        const deadline = Date.now() + timeoutMs;
        while (!output.includes(marker)) {
          const remaining = deadline - Date.now();
          if (remaining <= 0) {
            throw new Error(`timed out waiting for '${marker}': ${output}`);
          }
          const chunk = await Promise.race([
            reader.read(),
            new Promise<never>((_, reject) =>
              setTimeout(
                () =>
                  reject(
                    new Error(`timed out waiting for '${marker}': ${output}`),
                  ),
                remaining,
              )
            ),
          ]);
          if (chunk.done) {
            throw new Error(`process ended before '${marker}': ${output}`);
          }
          output += chunk.value;
        }
      };

      try {
        await readUntil("rust resource growth connected");
        // Grow: approve the optional resource the deployment declined. The Rust
        // handle must adopt it automatically; no refresh call exists.
        await runtime.contracts.apply({ contract });
        await readUntil("rust resource growth complete");
        const status = await child.status;
        assert(status.success, output);
      } finally {
        try {
          await child.stdin?.close();
        } catch {
          // stdin may already be closed once the process exited.
        }
        try {
          child.kill("SIGKILL");
        } catch {
          // the process may have exited already.
        }
      }
    }, runtimeOptions);
  },
);
