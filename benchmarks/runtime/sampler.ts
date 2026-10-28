// Sample owned benchmark descendants, not unrelated processes or host totals.
import { existsSync } from "node:fs";
import { appendFile, readdir, readFile, writeFile } from "node:fs/promises";
import { setTimeout as sleep } from "node:timers/promises";

const [rootArgument, output, ticksArgument] = process.argv.slice(2);
const rootPid = Number(rootArgument);
const ticks = Number(ticksArgument);
const stream = `${output}/resources.jsonl`;
await writeFile(stream, "");

function role(pid: number, command: string) {
  if (pid === rootPid) return "coordinator";
  if (command.includes("trellis-server")) return "trellis-server";
  if (command.includes("nats-server")) return "nats";
  if (command.includes("benchmarks/runtime/http.ts")) return "http-provider";
  if (command.includes("worker.ts")) {
    return command.includes("--provider") ? "provider" : "load-generator";
  }
  if (command.includes("trellis-performance")) {
    if (command.includes("--provider")) return "provider";
    return command.includes("--http-provider")
      ? "http-provider"
      : "load-generator";
  }
  return "setup-child";
}

while (!existsSync(`${output}/stop-sampler`)) {
  const processes = new Map<number, string[]>();
  for (const name of await readdir("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    // A process may exit between listing and reading.
    const stat = await readFile(`/proc/${name}/stat`, "utf8").catch(() => null);
    if (stat) processes.set(Number(name), stat.split(") ").at(-1)!.split(" "));
  }
  const owned = new Set([rootPid]);
  for (let grew = true; grew;) {
    grew = false;
    for (const [pid, stat] of processes) {
      if (!owned.has(pid) && owned.has(Number(stat[1]))) {
        owned.add(pid);
        grew = true;
      }
    }
  }
  const phase = (
    await readFile(`${output}/phase.txt`, "utf8").catch(() => "setup")
  ).trim();
  const rows = [];
  for (const pid of [...owned].sort((a, b) => a - b)) {
    const stat = processes.get(pid);
    if (!stat || pid === process.pid) continue;
    try {
      const command = (
        await readFile(`/proc/${pid}/cmdline`, "utf8")
      ).replaceAll("\0", " ");
      const memory: Record<string, number> = {};
      for (const line of (
        await readFile(`/proc/${pid}/smaps_rollup`, "utf8")
      ).split("\n")) {
        const match = line.match(
          /^(Rss|Pss|Private_Clean|Private_Dirty):\s+(\d+)/,
        );
        if (match) memory[`${match[1]}KiB`] = Number(match[2]);
      }
      rows.push({
        pid,
        startTicks: Number(stat[19]),
        role: role(pid, command),
        userCpuSeconds: Number(stat[11]) / ticks,
        systemCpuSeconds: Number(stat[12]) / ticks,
        threads: Number(stat[17]),
        fds: (await readdir(`/proc/${pid}/fd`)).length,
        ...memory,
      });
    } catch {
      continue;
    }
  }
  await appendFile(
    stream,
    `${JSON.stringify({ unixMs: Date.now(), phase, processes: rows })}\n`,
  );
  await sleep(250);
}
