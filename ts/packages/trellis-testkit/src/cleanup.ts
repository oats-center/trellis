import { join } from "@std/path";

let hasProc: boolean | undefined;

async function canCheckProcesses(): Promise<boolean> {
  if (hasProc !== undefined) return hasProc;
  try {
    const stat = await Deno.stat("/proc");
    hasProc = stat.isDirectory;
  } catch {
    hasProc = false;
  }
  return hasProc;
}

async function psProbe(args: string[]): Promise<string> {
  const result = await new Deno.Command("ps", {
    args,
    stdout: "piped",
    stderr: "null",
  }).output();
  return new TextDecoder().decode(result.stdout).trim();
}

/** @internal Returns true only when the pid no longer names a live process. */
export async function processIsGone(pid: number): Promise<boolean> {
  if (await canCheckProcesses()) {
    try {
      await Deno.stat(`/proc/${pid}`);
      return false;
    } catch (error) {
      if (error instanceof Deno.errors.NotFound) return true;
      return false;
    }
  }
  // No /proc (macOS): probe with ps so stale cleanup also runs there.
  try {
    return await psProbe(["-p", String(pid), "-o", "pid="]) === "";
  } catch {
    return false; // cannot probe; assume the pid is still in use
  }
}

function parsePid(value: string): number | undefined {
  const pid = Number(value.trim());
  return Number.isSafeInteger(pid) && pid > 0 ? pid : undefined;
}

/** @internal Start identity of a spawned child used to verify stale pid files. */
export type ProcessIdentity = {
  readonly pid: number;
  /** Process start time: `/proc/<pid>/stat` field 22 on linux, `ps -o lstart=` on darwin. */
  readonly start: string;
  /** Absolute executable path recorded at spawn (empty when unknown). */
  readonly executable: string;
};

/** @internal Reads the start time of a live process from `/proc/<pid>/stat` field 22. */
async function linuxProcessStartTime(pid: number): Promise<string | undefined> {
  try {
    const stat = await Deno.readTextFile(`/proc/${pid}/stat`);
    // `comm` is the parenthesized field and may contain spaces; the fields
    // after it are whitespace-separated and field 22 (`starttime`) is at
    // index 22 - 3 = 19.
    const fields = stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/);
    return fields[19];
  } catch {
    return undefined;
  }
}

/** @internal Records the start identity of a freshly spawned child process. */
export async function recordProcessIdentity(
  pid: number,
  executable: string,
): Promise<ProcessIdentity> {
  if (await canCheckProcesses()) {
    return {
      pid,
      start: (await linuxProcessStartTime(pid)) ?? "",
      executable,
    };
  }
  try {
    return {
      pid,
      start: await psProbe(["-p", String(pid), "-o", "lstart="]),
      executable,
    };
  } catch {
    return { pid, start: "", executable };
  }
}

/** @internal True when the first command-line token names the recorded executable. */
export function commandExecutableMatches(
  command: string,
  executable: string,
): boolean {
  const token = command.trimStart().split(/\s+/, 1)[0] ?? "";
  if (token === "") return false;
  const basename = executable.split("/").at(-1) ?? executable;
  return token === executable || token === basename;
}

/** @internal Returns true only when the live process at the pid matches the recorded identity. */
export async function processMatchesIdentity(
  identity: ProcessIdentity,
): Promise<boolean> {
  if (await canCheckProcesses()) {
    if (identity.start === "") return false; // no recorded identity: never signal
    const current = await linuxProcessStartTime(identity.pid);
    return current !== undefined && current === identity.start;
  }
  // darwin: no /proc start times. The recorded start time must equal the
  // current one AND the first command-line token must name the recorded
  // executable exactly (a substring match could kill an unrelated process).
  try {
    const lstart = await psProbe(["-p", String(identity.pid), "-o", "lstart="]);
    if (lstart === "" || lstart !== identity.start) return false;
    if (identity.executable === "") return false;
    const command = await psProbe([
      "-p",
      String(identity.pid),
      "-o",
      "command=",
    ]);
    return commandExecutableMatches(command, identity.executable);
  } catch {
    return false;
  }
}

/** @internal Renders a pid file carrying a child pid, start identity, and executable. */
export function formatPidFile(identity: ProcessIdentity): string {
  return `${identity.pid}\n${identity.start}\n${identity.executable}\n`;
}

/** @internal Parses a pid file written by `formatPidFile`. */
export function parsePidFile(content: string): ProcessIdentity | undefined {
  const lines = content.split("\n");
  const pid = parsePid(lines[0] ?? "");
  if (pid === undefined) return undefined;
  return {
    pid,
    start: (lines[1] ?? "").trim(),
    executable: (lines[2] ?? "").trim(),
  };
}

/** @internal Writes the owning process marker used to reap abandoned test directories. */
export async function writeTrellisTestOwnerMarker(
  dir: string,
  markerName: string,
): Promise<void> {
  await Deno.writeTextFile(join(dir, markerName), `${Deno.pid}\n`);
}

/** @internal Removes marked test directories whose owner process is gone. */
export async function removeStaleMarkedDirectories(args: {
  readonly parent: string;
  readonly prefix: string;
  readonly markerName: string;
}): Promise<void> {
  const entries: Deno.DirEntry[] = [];
  try {
    for await (const entry of Deno.readDir(args.parent)) entries.push(entry);
  } catch {
    return;
  }

  for (const entry of entries) {
    if (!entry.isDirectory || !entry.name.startsWith(args.prefix)) continue;
    const path = join(args.parent, entry.name);
    const marker = await Deno.readTextFile(join(path, args.markerName)).catch(
      () => undefined,
    );
    const pid = marker === undefined ? undefined : parsePid(marker);
    if (pid !== undefined && await processIsGone(pid)) {
      await Deno.remove(path, { recursive: true }).catch(() => undefined);
    }
  }
}

/** @internal Removes PID-named resources whose owner process is gone. */
export async function removeStalePidNamedResources(args: {
  readonly names: readonly string[];
  readonly prefix: string;
  readonly remove: (name: string) => Promise<void>;
}): Promise<void> {
  for (const name of args.names) {
    if (!name.startsWith(args.prefix)) continue;
    const pid = parsePid(name.slice(args.prefix.length).split("-", 1)[0]);
    if (pid !== undefined && await processIsGone(pid)) await args.remove(name);
  }
}
