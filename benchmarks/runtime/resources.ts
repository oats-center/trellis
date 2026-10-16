import { readFile } from "node:fs/promises";
import { z } from "zod";

const identities = z.array(
  z.object({ pid: z.number().int().positive(), role: z.string() }),
);

/** Exact workload-window CPU counters; periodic samples are only a memory timeline. */
export async function snapshotResources(
  output: string,
  ticks: number,
  selfRole = "load-generator",
) {
  const processes = identities.parse(
    JSON.parse(await readFile(`${output}/processes.json`, "utf8")),
  );
  processes.push({ pid: process.pid, role: selfRole });
  return await Promise.all(
    processes.map(async ({ pid, role }) => {
      const path = `/proc/${pid}`;
      const stat = (await readFile(`${path}/stat`, "utf8"))
        .split(") ")
        .at(-1)!
        .trim()
        .split(/\s+/);
      const memory = await readFile(`${path}/smaps_rollup`, "utf8");
      return {
        pid,
        role,
        startTicks: Number(stat[19]),
        userCpuSeconds: Number(stat[11]) / ticks,
        systemCpuSeconds: Number(stat[12]) / ticks,
        rssKiB: Number(memory.match(/^Rss:\s+(\d+)/m)?.[1]),
        pssKiB: Number(memory.match(/^Pss:\s+(\d+)/m)?.[1]),
      };
    }),
  );
}
