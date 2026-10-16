import {
  spawn,
  type ChildProcess,
  type ChildProcessWithoutNullStreams,
  type SpawnOptions,
  type SpawnOptionsWithoutStdio,
} from "node:child_process";

/** Starts a real Node child and immediately retains completion, even if it exits before a caller waits. */
export function spawnTracked(
  command: string,
  args: readonly string[],
  options?: SpawnOptionsWithoutStdio,
): ChildProcessWithoutNullStreams & { exited: Promise<ProcessExit> };
/** Starts a child with explicit stdio, whose streams may be absent. */
export function spawnTracked(
  command: string,
  args: readonly string[],
  options: SpawnOptions,
): ChildProcess & { exited: Promise<ProcessExit> };
export function spawnTracked(
  command: string,
  args: readonly string[],
  options: SpawnOptions = {},
) {
  const child = spawn(command, [...args], options);
  const exited = new Promise<ProcessExit>((resolve, reject) => {
    child.once("close", (code, signal) => resolve({ code, signal }));
    child.once("error", reject);
  });
  void exited.catch(() => {});
  return Object.assign(child, { exited });
}

/** A real Node subprocess with its completion event retained from creation. */
export type TrackedProcess = ReturnType<typeof spawnTracked>;

/** The completion status of an ordinary Node child. */
export type ProcessExit = {
  code: number | null;
  signal: NodeJS.Signals | null;
};
