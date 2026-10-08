// External Node inspector control only; the provider and SDK are normal builds.
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { resolve } from "node:path";

const child = spawn(process.execPath, [
  "--inspect-brk=127.0.0.1:0",
  resolve(
    process.env.TRELLIS_WORKER_CONSUMER_DIR,
    "worker-lifecycle-provider.mjs",
  ),
], { stdio: ["ignore", "pipe", "pipe"] });
let stderr = "";
const ready = new Promise((resolve) =>
  child.stdout.on("data", (chunk) => {
    if (String(chunk).includes("worker-probe-ready")) resolve();
  })
);
child.stdout.on("data", (chunk) => {
  process.stderr.write(chunk);
  if (String(chunk).includes("worker-probe-exited")) {
    console.log(JSON.stringify({ event: "exited" }));
  }
});
child.stderr.on("data", (chunk) => {
  stderr += chunk;
  process.stderr.write(chunk);
});
const url = await new Promise((resolve, reject) => {
  child.stderr.on("data", () => {
    const match = stderr.match(/ws:\/\/[^\s]+/);
    if (match) resolve(match[0]);
  });
  child.on("exit", (code) => reject(new Error(`provider exited ${code}`)));
});
const socket = new WebSocket(url);
await new Promise((resolve, reject) => {
  socket.onopen = resolve;
  socket.onerror = reject;
});
let sequence = 0;
const pending = new Map(),
  workerPending = new Map(),
  workers = [],
  pauses = new Map();
let holdGrowingWorkers = false;
let holdStartingWorker = false;
function call(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`timeout ${method}`));
    }, 10_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
}
function workerCall(sessionId, method, params = {}) {
  const id = ++sequence, key = `${sessionId}:${id}`;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      workerPending.delete(key);
      reject(new Error(`timeout worker ${method}`));
    }, 10_000);
    workerPending.set(key, { resolve, reject, timer });
    void call("NodeWorker.sendMessageToWorker", {
      sessionId,
      message: JSON.stringify({ id, method, params }),
    }).catch(reject);
  });
}
socket.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.method === "Runtime.exceptionThrown") {
    console.error(JSON.stringify(message));
  }
  if (message.method === "Debugger.paused") {
    void call("Debugger.evaluateOnCallFrame", {
      callFrameId: message.params.callFrames[0].callFrameId,
      expression:
        "JSON.stringify({contextDigest:entry.contextDigest,revokedAt:entry.revokedAt})",
      returnByValue: true,
    }).then(async (result) => {
      console.log(
        JSON.stringify({ event: "revoked", value: result.result.value }),
      );
      await call("Debugger.resume");
    });
  }
  if (message.method === "NodeWorker.attachedToWorker") {
    workers.push(message.params);
    void (async () => {
      const worker = message.params;
      if (
        holdGrowingWorkers &&
        /verification-ordinary-/.test(worker.workerInfo.title)
      ) {
        await workerCall(worker.sessionId, "Debugger.enable");
        await workerCall(worker.sessionId, "Debugger.setBreakpointByUrl", {
          url: worker.workerInfo.url,
          lineNumber: 74,
        });
      }
      await workerCall(worker.sessionId, "Runtime.runIfWaitingForDebugger");
      console.log(
        JSON.stringify({ event: "attached", title: worker.workerInfo.title }),
      );
    })();
  }
  if (message.method === "NodeWorker.receivedMessageFromWorker") {
    const inner = JSON.parse(message.params.message),
      key = `${message.params.sessionId}:${inner.id}`;
    if (inner.method === "Debugger.paused") {
      if (inner.params.reason === "Break on start") {
        if (holdStartingWorker) {
          pauses.set(message.params.sessionId, inner.params);
          console.log(
            JSON.stringify({
              event: "startup-paused",
              worker: message.params.sessionId,
            }),
          );
          return;
        }
        void workerCall(message.params.sessionId, "Debugger.resume");
        return;
      }
      pauses.set(message.params.sessionId, inner.params);
      console.log(
        JSON.stringify({
          event: "paused",
          worker: message.params.sessionId,
          frame: inner.params.callFrames[0].location,
          function: inner.params.callFrames[0].functionName,
          reason: inner.params.reason,
        }),
      );
    }
    const task = workerPending.get(key);
    if (task) {
      workerPending.delete(key);
      clearTimeout(task.timer);
      inner.error
        ? task.reject(new Error(JSON.stringify(inner.error)))
        : task.resolve(inner.result);
    }
  }
  if (message.method === "NodeWorker.detachedFromWorker") {
    const worker = workers.find((worker) =>
      worker.sessionId === message.params.sessionId
    );
    if (worker) worker.lost = true;
  }
  const task = pending.get(message.id);
  if (task) {
    pending.delete(message.id);
    clearTimeout(task.timer);
    message.error
      ? task.reject(new Error(JSON.stringify(message.error)))
      : task.resolve(message.result);
  }
};
try {
  await call("Runtime.enable");
  await call("NodeWorker.enable", { waitForDebuggerOnStart: true });
  await call("Runtime.runIfWaitingForDebugger");
  let readyTimer;
  try {
    await Promise.race([
      ready,
      new Promise((_, reject) => {
        readyTimer = setTimeout(
          () => reject(new Error("provider did not become ready")),
          45_000,
        );
      }),
    ]);
  } finally {
    clearTimeout(readyTimer);
  }
  const ordinary = workers.filter((worker) =>
    /verification-ordinary-/.test(worker.workerInfo.title)
  );
  if (ordinary.length !== 1) {
    throw new Error(
      `ordinary verifier discovery failed: ${JSON.stringify(workers)}`,
    );
  }
  for (const worker of ordinary) {
    await workerCall(worker.sessionId, "Debugger.enable");
    // Current producer-built worker: after proof verification, before reply.
    await workerCall(worker.sessionId, "Debugger.setBreakpointByUrl", {
      url: worker.workerInfo.url,
      lineNumber: 74,
    });
  }
  console.log(JSON.stringify({ event: "ready" }));
  for await (const line of createInterface({ input: process.stdin })) {
    const { command, contextDigest } = JSON.parse(line);
    const sessionId = command === "exit-starting"
      ? [...pauses.entries()].find(([, pause]) =>
        pause.reason === "Break on start"
      )?.[0]
      : [...pauses.keys()][0];
    if (command === "resume") {
      holdGrowingWorkers = false;
      holdStartingWorker = false;
      for (
        const worker of workers.filter((worker) =>
          /verification-ordinary-/.test(worker.workerInfo.title)
        )
      ) {
        if (!worker.lost) {
          await workerCall(worker.sessionId, "Debugger.disable");
        }
      }
      pauses.clear();
    } else if (command === "hold-growth" || command === "hold-startup") {
      holdGrowingWorkers = true;
      holdStartingWorker = command === "hold-startup";
      console.log(JSON.stringify({ event: "holding-growth" }));
    } else if (command === "count") {
      console.log(
        JSON.stringify({
          event: "count",
          value: String(
            workers.filter((worker) =>
              /verification-ordinary-/.test(worker.workerInfo.title)
            ).length,
          ),
        }),
      );
    } else if (command === "exit" || command === "exit-starting") {
      const prototype = await call("Runtime.evaluate", {
        expression:
          "process.getBuiltinModule('node:worker_threads').Worker.prototype",
      });
      const objects = await call("Runtime.queryObjects", {
        prototypeObjectId: prototype.result.objectId,
      });
      const selected = workers.find((worker) => worker.sessionId === sessionId);
      const result = await call("Runtime.callFunctionOn", {
        objectId: objects.objects.objectId,
        functionDeclaration:
          "function(id){const worker=this.find(worker=>{try{return worker.threadId===id}catch{return false}});if(!worker)throw new Error('worker not found');worker.once('exit',()=>console.log('worker-probe-exited'));worker.terminate();return true;}",
        arguments: [{ value: Number(selected.workerInfo.workerId) }],
        returnByValue: true,
      });
      if (result.exceptionDetails) {
        throw new Error(JSON.stringify(result.exceptionDetails));
      }
      // A debugger attachment otherwise keeps the terminating thread alive.
      await call("NodeWorker.detach", { sessionId });
    } else if (command === "watch-revocation") {
      await call("Debugger.enable");
      // Current npm build: shared entry marked revoked, before notification.
      await call("Debugger.setBreakpointByUrl", {
        urlRegex: "auth/authorization/provider_cache\\.js$",
        lineNumber: 1724,
        condition: `entry.contextDigest === ${JSON.stringify(contextDigest)}`,
      });
      console.log(JSON.stringify({ event: "watching" }));
    } else if (command === "inspect") {
      const frame = pauses.get(sessionId).callFrames[0];
      const result = await workerCall(
        sessionId,
        "Debugger.evaluateOnCallFrame",
        {
          callFrameId: frame.callFrameId,
          expression: "JSON.stringify({iat:message.request.iat,policy,parsed})",
          returnByValue: true,
        },
      );
      console.log(
        JSON.stringify({ event: "proof", value: result.result.value }),
      );
    } else if (command === "stop") break;
  }
} finally {
  socket.close();
  child.kill();
}
