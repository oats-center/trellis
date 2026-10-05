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
    JSON.parse(await Deno.readTextFile(`${output}/processes.json`)),
  );
  processes.push({ pid: Deno.pid, role: selfRole });
  return await Promise.all(processes.map(async ({ pid, role }) => {
    const path = `/proc/${pid}`;
    const stat = (await Deno.readTextFile(`${path}/stat`)).split(") ").at(-1)!
      .trim().split(/\s+/);
    const memory = await Deno.readTextFile(`${path}/smaps_rollup`);
    return {
      pid,
      role,
      startTicks: Number(stat[19]),
      userCpuSeconds: Number(stat[11]) / ticks,
      systemCpuSeconds: Number(stat[12]) / ticks,
      rssKiB: Number(memory.match(/^Rss:\s+(\d+)/m)?.[1]),
      pssKiB: Number(memory.match(/^Pss:\s+(\d+)/m)?.[1]),
    };
  }));
}
