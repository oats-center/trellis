// Capture a diagnostic CPU profile from a benchmark worker's Deno inspector.
import { z } from "zod";

const output = z.string().parse(Deno.args[0]);
const port = z.coerce.number().int().min(1).max(65535).parse(
  Deno.args[1] ?? 9229,
);
const role = z.enum(["client", "provider"]).parse(Deno.args[2] ?? "client");
const seconds = z.coerce.number().positive().max(60).parse(Deno.args[3] ?? 15);
const deadline = Date.now() + 120_000;
let endpoint = "";
while (!endpoint && Date.now() < deadline) {
  try {
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`))
      .json();
    endpoint = targets[0]?.webSocketDebuggerUrl ?? "";
  } catch {
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}
if (!endpoint) throw new Error(`${role} inspector did not start`);
const socket = new WebSocket(endpoint);
let id = 0;
const pending = new Map<number, {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}>();
socket.onmessage = (event) => {
  const message = JSON.parse(event.data);
  const call = pending.get(message.id);
  if (!call) return;
  pending.delete(message.id);
  if (message.error) call.reject(new Error(JSON.stringify(message.error)));
  else call.resolve(message.result);
};
socket.onclose = () => {
  for (const call of pending.values()) {
    call.reject(new Error("Inspector closed"));
  }
  pending.clear();
};
function call(method: string): Promise<unknown> {
  return new Promise((resolve, reject) => {
    if (socket.readyState !== WebSocket.OPEN) {
      reject(new Error("Inspector is not connected"));
      return;
    }
    const requestId = ++id;
    pending.set(requestId, { resolve, reject });
    socket.send(JSON.stringify({ id: requestId, method, params: {} }));
  });
}
try {
  await new Promise<void>((resolve, reject) => {
    socket.onopen = () => resolve();
    socket.onerror = () => reject(new Error("Inspector connection failed"));
  });
  await call("Profiler.enable");
  let measuring = false;
  while (Date.now() < deadline) {
    try {
      measuring = (await Deno.readTextFile(`${output}/phase.txt`)).includes(
        "echo-window",
      );
      if (measuring) break;
    } catch (error) {
      if (!(error instanceof Deno.errors.NotFound)) throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (!measuring) throw new Error("Benchmark did not enter an offered window");
  await Deno.writeTextFile(
    `${output}/inspector-targets.json`,
    JSON.stringify(
      await (await fetch(`http://127.0.0.1:${port}/json/list`)).json(),
    ),
  );
  await call("Profiler.start");
  await new Promise((resolve) => setTimeout(resolve, seconds * 1000));
  const profile = await call("Profiler.stop");
  await Deno.writeTextFile(
    `${output}/${role}.cpuprofile.json`,
    JSON.stringify(profile),
  );
} finally {
  socket.close();
}
