import type { Worker as NodeWorker } from "node:worker_threads";
import { z } from "zod";
import {
  REFUSAL_REQUEST_LIMIT,
  type RequestLimits,
  RequestLimitsSchema,
} from "../../request_admission.ts";
import {
  recordCatalogCounter,
  recordCatalogDuration,
  recordCatalogUpDown,
} from "../../telemetry/metrics.ts";
import {
  type AuthorizationIssuerKey,
  type TransferFrameDescriptor,
  type VerifyAuthorizationEventArgs,
  type VerifyAuthorizationEventResult,
  type VerifyAuthorizationRequestArgs,
  type VerifyAuthorizationRequestResult,
  wasmVerificationPolicy,
} from "../protocol_wasm.ts";
import { base64urlDecode } from "../utils.ts";

/** Trusted local scheduling class; never selected by a proof header. @internal */
export type VerificationLane = "ordinary" | "control" | "refusal";
type SchedulingLane = VerificationLane | "data";
type ContextJob = {
  entry: object;
  context: { issuer: AuthorizationIssuerKey; signed: unknown; digest: string };
  policy: () => VerifyAuthorizationRequestArgs["policy"];
};
type RequestJob = ContextJob & {
  kind: "verify";
  input: Omit<VerifyAuthorizationRequestArgs, "contextHandle" | "policy">;
  payload: Uint8Array;
  transfer?: {
    transferId: string;
    providerConnectionId: string;
    consumerConnectionId: string;
  };
};
type EventJob = ContextJob & {
  kind: "event";
  input: Omit<VerifyAuthorizationEventArgs, "contextHandle" | "policy">;
  payload: Uint8Array;
};
/** Exact-byte proof work; coordinates are supplied by the owning protocol session. @internal */
export type FrameDigestJob =
  | {
    kind: "live-digest";
    contextDigest: string;
    subject: string;
    payload: Uint8Array;
  }
  | {
    kind: "transfer-digest";
    contextDigest: string;
    subject: string;
    payload: Uint8Array;
    descriptor: TransferFrameDescriptor;
  }
  | {
    kind: "transfer-frame-digest";
    payload: Uint8Array;
    descriptor: TransferFrameDescriptor;
  };
/** Provider-proof verification; never chooses a reserved lane from untrusted headers. @internal */
export type FrameVerifyJob =
  | {
    kind: "live-verify";
    contextDigest: string;
    subject: string;
    payload: Uint8Array;
    proof: string;
    providerKey: string;
  }
  | {
    kind: "transfer-verify";
    contextDigest: string;
    subject: string;
    payload: Uint8Array;
    proof: string;
    providerKey: string;
    descriptor: TransferFrameDescriptor;
  };
type Job = RequestJob | EventJob | FrameDigestJob | FrameVerifyJob;
type JobResult =
  | { kind: "verify"; value: VerifyAuthorizationRequestResult }
  | { kind: "event"; value: VerifyAuthorizationEventResult }
  | { kind: "digest"; value: string }
  | { kind: "verified" };
type Reply = {
  ready?: boolean;
  id?: number;
  error?: string;
  result?: JobResult;
  timing?: { totalMs: number; verifyMs: number };
};
type Task = {
  id: number;
  lane: SchedulingLane;
  job: Job;
  contextId?: string;
  settled: ReturnType<
    typeof Promise.withResolvers<JobResult>
  >;
  timer: ReturnType<typeof setTimeout>;
  queuedAt: number;
  sentAt: number;
};
type Slot = {
  worker: Worker | NodeWorker;
  ready: ReturnType<typeof Promise.withResolvers<void>>;
  queue: Task[];
  active?: Task;
  copiedBytes: number;
  installed: Set<string>;
  releases: Map<
    number,
    {
      done: ReturnType<typeof Promise.withResolvers<void>>;
      timer: ReturnType<typeof setTimeout>;
    }
  >;
  failed?: Error;
  started: boolean;
  lane: "ordinary" | "reserved";
};

/** Grow-only ordinary WASM verifiers with independent reserved capacity. @internal */
export class AuthorizationVerificationWorkers {
  readonly #slots: Slot[] = [];
  readonly #contextIds = new WeakMap<object, string>();
  readonly #counts: Record<SchedulingLane, number> = {
    ordinary: 0,
    control: 0,
    refusal: 0,
    data: 0,
  };
  readonly #limits: Record<SchedulingLane, number>;
  #dataCapacity = Promise.withResolvers<void>();
  #sequence = 0;
  #closed = false;
  #growthTimer?: ReturnType<typeof setTimeout>;
  #behindSince?: number;
  #starting = false;
  #growthFailed = false;

  private constructor(
    readonly timeoutMs: number,
    limits?: RequestLimits,
    readonly maxOrdinaryWorkers = 3,
    readonly queueAgeMs = 20,
    readonly backlogDurationMs = 100,
  ) {
    const parsed = RequestLimitsSchema.parse(limits ?? {});
    this.#limits = {
      ordinary: parsed.requests,
      control: parsed.controls,
      refusal: REFUSAL_REQUEST_LIMIT,
      data: parsed.requests,
    };
  }

  /** Start one ordinary and one reserved worker; initial failures reject configuration. */
  static async open(
    timeoutMs: number,
    limits?: RequestLimits,
    maxOrdinaryWorkers = 3,
    queueAgeMs = 20,
    backlogDurationMs = 100,
    reserved = true,
  ): Promise<AuthorizationVerificationWorkers> {
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
      throw new Error("Verification worker timeout must be positive");
    }
    z.number().int().positive().max(Number.MAX_SAFE_INTEGER).parse(
      maxOrdinaryWorkers,
    );
    z.number().finite().nonnegative().parse(queueAgeMs);
    z.number().finite().nonnegative().parse(backlogDurationMs);
    const pool = new AuthorizationVerificationWorkers(
      timeoutMs,
      limits,
      maxOrdinaryWorkers,
      queueAgeMs,
      backlogDurationMs,
    );
    try {
      await pool.#start("ordinary");
      if (reserved) await pool.#start("reserved");
      return pool;
    } catch (error) {
      pool.close();
      throw error;
    }
  }

  async #start(lane: Slot["lane"]): Promise<Slot> {
    if (this.#closed) throw new Error("Verification workers closed");
    const index = this.#slots.length;
    const startedAt = performance.now();
    const runtime = globalThis as Record<string, unknown>;
    const node = !runtime.Deno &&
      !!(runtime.process as { versions?: { node?: string } } | undefined)
        ?.versions?.node;
    const url = new URL("./verification_worker.mjs", import.meta.url);
    const worker = node
      ? new (await import("node:worker_threads")).Worker(url, {
        name: `trellis-verification-${lane}-${index}`,
      })
      : new Worker(new URL("./verification_worker.mjs", import.meta.url), {
        type: "module",
        name: `trellis-verification-${lane}-${index}`,
      });
    const slot: Slot = {
      worker,
      ready: Promise.withResolvers<void>(),
      queue: [],
      installed: new Set(),
      releases: new Map(),
      copiedBytes: 0,
      started: false,
      lane,
    };
    this.#slots.push(slot);
    const onMessage = (reply: Reply) => this.#receive(slot, reply);
    if ("on" in worker) {
      worker.on("message", onMessage);
      worker.on(
        "error",
        (error) =>
          this.#fail(
            slot,
            error instanceof Error ? error : new Error(String(error)),
          ),
      );
      worker.on("messageerror", (error) => this.#fail(slot, error));
      worker.on(
        "exit",
        (code) =>
          this.#fail(
            slot,
            new Error(`Verification worker exited (${code})`),
          ),
      );
    } else {
      worker.onmessage = (event: MessageEvent<Reply>) => onMessage(event.data);
      worker.onerror = (event) => {
        event.preventDefault();
        this.#fail(slot, new Error(event.message));
      };
      worker.onmessageerror = () =>
        this.#fail(
          slot,
          new Error("Verification worker message could not be decoded"),
        );
    }
    const timer = setTimeout(
      () =>
        this.#fail(
          slot,
          new Error("Verification worker startup timed out"),
        ),
      this.timeoutMs,
    );
    try {
      if (this.#closed) {
        this.#fail(slot, new Error("Verification workers closed"));
      }
      await slot.ready.promise;
      if (slot.failed || this.#closed) {
        throw slot.failed ?? new Error("Verification workers closed");
      }
      slot.started = true;
      recordCatalogUpDown("trellis.auth.worker.active", 1, {
        "trellis.kind": slot.lane,
      });
      recordCatalogDuration(
        "trellis.auth.verification.duration",
        performance.now() - startedAt,
        {
          "trellis.kind": lane === "ordinary" ? "ordinary" : "control",
          "trellis.phase": "worker.start",
        },
      );
    } finally {
      clearTimeout(timer);
    }
    return slot;
  }

  /** Schedule only bounded work; copies are created when a worker starts the task. */
  async verify(args: {
    entry: object;
    lane: VerificationLane;
    context: ContextJob["context"];
    input: RequestJob["input"];
    policy: ContextJob["policy"];
    transfer?: RequestJob["transfer"];
  }): Promise<VerifyAuthorizationRequestResult> {
    const result = await this.#schedule({
      ...args,
      kind: "verify",
      payload: args.input.payload,
    }, args.lane);
    if (result.kind !== "verify") {
      throw new Error("Unexpected request verification result");
    }
    return result.value;
  }

  /** Verify events on the same bounded ordinary workers, with historical contexts. */
  async verifyEvent(
    args: ContextJob & { input: EventJob["input"] },
  ): Promise<VerifyAuthorizationEventResult> {
    const result = await this.#schedule({
      ...args,
      kind: "event",
      payload: args.input.payload,
    }, "data");
    if (result.kind !== "event") {
      throw new Error("Unexpected event verification result");
    }
    return result.value;
  }

  /** Compute the existing Rust protocol digest without transferring private keys. */
  async digest(job: FrameDigestJob): Promise<Uint8Array> {
    const result = await this.#schedule(job, "data");
    if (result.kind !== "digest") {
      throw new Error("Unexpected frame digest result");
    }
    return base64urlDecode(result.value);
  }

  /** Verify a provider signature over its exact subject, coordinates, and bytes. */
  async verifyFrame(job: FrameVerifyJob): Promise<void> {
    const result = await this.#schedule(job, "data");
    if (result.kind !== "verified") {
      throw new Error("Unexpected frame verification result");
    }
  }

  async #schedule(job: Job, lane: SchedulingLane): Promise<JobResult> {
    // Data sources already await each frame/event inside their existing bounded
    // protocol lifecycle. Backpressure those sources, never steal RPC admission
    // or reserved capacity, and do not copy a waiting payload into the worker.
    if (lane === "data") {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const deadline = new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("Data verification scheduling timed out")),
          this.timeoutMs,
        );
      });
      try {
        while (!this.#closed && this.#counts.data >= this.#limits.data) {
          await Promise.race([this.#dataCapacity.promise, deadline]);
        }
      } finally {
        clearTimeout(timer);
      }
    }
    if (this.#closed) throw new Error("Verification workers closed");
    const slot = lane === "ordinary" || lane === "data" ||
        !this.#slots.some((slot) => slot.lane === "reserved")
      ? this.#slots.filter((slot) => slot.lane === "ordinary" && slot.started)
        .reduce<Slot | undefined>(
          (selected, candidate) =>
            candidate.failed ? selected : !selected ||
                candidate.queue.length + Number(!!candidate.active) <
                  selected.queue.length + Number(!!selected.active)
              ? candidate
              : selected,
          undefined,
        )
      : this.#slots.find((slot) => slot.lane === "reserved");
    if (!slot || slot.failed) {
      throw slot?.failed ?? new Error("No available verification worker");
    }
    if (this.#counts[lane] >= this.#limits[lane]) {
      throw new Error("Verification scheduling capacity exhausted");
    }
    let contextId: string | undefined;
    if ("entry" in job) {
      contextId = this.#contextIds.get(job.entry);
      if (!contextId) {
        contextId = String(++this.#sequence);
        this.#contextIds.set(job.entry, contextId);
      }
    }
    const id = ++this.#sequence;
    const task: Task = {
      job,
      lane,
      contextId,
      id,
      queuedAt: performance.now(),
      sentAt: 0,
      settled: Promise.withResolvers<JobResult>(),
      timer: setTimeout(() => {
        const owner = this.#slots.find((candidate) =>
          candidate.active?.id === id ||
          candidate.queue.some((item) => item.id === id)
        );
        if (!owner) return;
        if (owner.active?.id === id) {
          this.#fail(
            owner,
            new Error("Verification worker execution timed out"),
          );
        } else {
          const index = owner.queue.findIndex((item) => item.id === id);
          if (index >= 0) {
            owner.queue.splice(index, 1);
            task.settled.reject(new Error("Verification scheduling timed out"));
            this.#considerGrowth();
          }
        }
      }, this.timeoutMs),
    };
    this.#counts[lane]++;
    recordCatalogUpDown("trellis.auth.worker.pending", 1, {
      "trellis.kind": lane,
    });
    slot.queue.push(task);
    this.#dispatch(slot);
    this.#considerGrowth();
    try {
      return await task.settled.promise;
    } finally {
      clearTimeout(task.timer);
      this.#counts[lane]--;
      if (lane === "data") {
        this.#dataCapacity.resolve();
        this.#dataCapacity = Promise.withResolvers<void>();
      }
      recordCatalogUpDown("trellis.auth.worker.pending", -1, {
        "trellis.kind": lane,
      });
    }
  }

  /** Release all copies of an entry's context; acknowledgments are bounded. */
  async release(entry: object): Promise<void> {
    const contextId = this.#contextIds.get(entry);
    if (!contextId) return;
    this.#contextIds.delete(entry);
    await Promise.all(this.#slots.map(async (slot) => {
      if (slot.failed || this.#closed) {
        return;
      }
      slot.installed.delete(contextId);
      const id = ++this.#sequence;
      const done = Promise.withResolvers<void>();
      const timer = setTimeout(
        () =>
          this.#fail(slot, new Error("Verification context release timed out")),
        this.timeoutMs,
      );
      slot.releases.set(id, { done, timer });
      try {
        slot.worker.postMessage({ kind: "release", id, contextId });
        await done.promise;
      } finally {
        clearTimeout(timer);
        slot.releases.delete(id);
      }
    }));
  }

  /** Fail unfinished verification closed and terminate all local workers. */
  close(): void {
    this.#closed = true;
    this.#dataCapacity.resolve();
    clearTimeout(this.#growthTimer);
    for (const slot of this.#slots) {
      this.#fail(slot, new Error("Verification workers closed"));
    }
  }

  #dispatch(slot: Slot): void {
    if (
      !slot.started || slot.active || slot.failed || this.#closed
    ) {
      return;
    }
    if (slot.lane === "ordinary" && !slot.queue.length) {
      // Use already available capacity before considering another thread.
      // Only queued, never attempted proofs may move between ordinary workers.
      const owner = this.#slots.filter((candidate) =>
        candidate.lane === "ordinary" && !candidate.failed &&
        candidate.queue.length
      )
        .reduce<Slot | undefined>(
          (oldest, candidate) =>
            !oldest || candidate.queue[0].queuedAt < oldest.queue[0].queuedAt
              ? candidate
              : oldest,
          undefined,
        );
      const next = owner?.queue.shift();
      if (next) slot.queue.push(next);
    }
    const control = slot.queue.findIndex((task) => task.lane === "control");
    const task = slot.queue.splice(control < 0 ? 0 : control, 1)[0];
    if (!task) return;
    slot.active = task;
    const dispatchedAt = performance.now();
    recordCatalogDuration(
      "trellis.auth.verification.duration",
      dispatchedAt - task.queuedAt,
      {
        "trellis.kind": task.lane,
        "trellis.phase": "worker.queue",
      },
    );
    try {
      const payload = new Uint8Array(task.job.payload).buffer;
      recordCatalogDuration(
        "trellis.auth.verification.duration",
        performance.now() - dispatchedAt,
        {
          "trellis.kind": task.lane,
          "trellis.phase": "worker.copy",
        },
      );
      let job;
      if (task.job.kind === "verify" || task.job.kind === "event") {
        const { payload: _, ...input } = task.job.input;
        job = {
          kind: task.job.kind,
          input,
          transfer: task.job.kind === "verify" ? task.job.transfer : undefined,
        };
      } else {
        const { payload: _, ...coordinates } = task.job;
        job = coordinates;
      }
      slot.copiedBytes = task.job.payload.byteLength;
      recordCatalogUpDown(
        "trellis.auth.worker.payload_bytes",
        task.job.payload.byteLength,
        { "trellis.kind": task.lane },
      );
      task.sentAt = performance.now();
      slot.worker.postMessage({
        ...job,
        id: task.id,
        contextId: task.contextId,
        context: "context" in task.job && task.contextId &&
            !slot.installed.has(task.contextId)
          ? task.job.context
          : undefined,
        policy: "policy" in task.job
          ? wasmVerificationPolicy(task.job.policy())
          : undefined,
        sentAt: Date.now(),
        payload,
      }, [payload]);
    } catch (error) {
      this.#fail(
        slot,
        error instanceof Error ? error : new Error(String(error)),
      );
    }
  }

  #receive(slot: Slot, reply: Reply): void {
    if (slot.failed) return;
    if (reply.ready) {
      slot.ready.resolve();
      return;
    }
    if (reply.id === undefined) {
      this.#fail(
        slot,
        new Error("Verification worker response has no task id"),
      );
      return;
    }
    const release = slot.releases.get(reply.id);
    if (release) {
      if (reply.error) release.done.reject(new Error(reply.error));
      else release.done.resolve();
      return;
    }
    const task = slot.active;
    if (!task || task.id !== reply.id) {
      this.#fail(
        slot,
        new Error("Verification worker task ownership mismatch"),
      );
      return;
    }
    slot.active = undefined;
    if (reply.timing) {
      for (
        const [phase, duration] of [
          ["worker.execute", reply.timing.totalMs],
          ["worker.verify", reply.timing.verifyMs],
          [
            "worker.boundary",
            Math.max(0, performance.now() - task.sentAt - reply.timing.totalMs),
          ],
        ] as const
      ) {
        recordCatalogDuration("trellis.auth.verification.duration", duration, {
          "trellis.kind": task.lane,
          "trellis.phase": phase,
        });
      }
    }
    recordCatalogUpDown(
      "trellis.auth.worker.payload_bytes",
      -slot.copiedBytes,
      { "trellis.kind": task.lane },
    );
    slot.copiedBytes = 0;
    if (reply.error) task.settled.reject(new Error(reply.error));
    else if (reply.result) {
      if (task.contextId) slot.installed.add(task.contextId);
      task.settled.resolve(reply.result);
    } else {task.settled.reject(
        new Error("Verification worker response has no result"),
      );}
    this.#dispatch(slot);
    this.#considerGrowth();
  }

  #considerGrowth(): void {
    clearTimeout(this.#growthTimer);
    this.#growthTimer = undefined;
    const ordinary = this.#slots.filter((slot) => slot.lane === "ordinary");
    const healthy = ordinary.filter((slot) => slot.started && !slot.failed);
    const oldest = Math.min(
      ...healthy.map((slot) =>
        slot.queue[0]?.queuedAt ?? Number.POSITIVE_INFINITY
      ),
    );
    const now = performance.now();
    if (
      this.#closed || this.#starting || this.#growthFailed ||
      ordinary.filter((slot) => !slot.failed).length >=
        this.maxOrdinaryWorkers ||
      !healthy.length || healthy.some((slot) => !slot.active) ||
      !Number.isFinite(oldest)
    ) {
      this.#behindSince = undefined;
      return;
    }
    if (now - oldest < this.queueAgeMs) {
      this.#behindSince = undefined;
      this.#growthTimer = setTimeout(
        () => this.#considerGrowth(),
        this.queueAgeMs - (now - oldest),
      );
      return;
    }
    this.#behindSince ??= now;
    if (now - this.#behindSince < this.backlogDurationMs) {
      this.#growthTimer = setTimeout(
        () => this.#considerGrowth(),
        this.backlogDurationMs - (now - this.#behindSince),
      );
      return;
    }
    this.#starting = true;
    this.#behindSince = undefined;
    void this.#start("ordinary").then((slot) => {
      if (this.#closed) return;
      // Only queued, never attempted proofs move to the newly ready worker.
      const queued = healthy.flatMap((owner) => owner.queue.splice(0)).sort((
        a,
        b,
      ) => a.queuedAt - b.queuedAt);
      const available = [...healthy.filter((owner) => !owner.failed), slot];
      for (const task of queued) {
        const owner = available.reduce((a, b) =>
          a.queue.length + Number(!!a.active) <=
              b.queue.length + Number(!!b.active)
            ? a
            : b
        );
        owner.queue.push(task);
      }
      for (const owner of available) this.#dispatch(owner);
    }).catch(() => {
      // Failed startup stays failed closed; healthy workers retain accepted work.
      // Avoid repeating startup against the same persistent backlog.
      this.#growthFailed = true;
    }).finally(() => {
      this.#starting = false;
      this.#considerGrowth();
    });
  }

  #fail(slot: Slot, error: Error): void {
    if (slot.failed) return;
    slot.failed = error;
    // Do not turn worker failure into an automatic restart loop.
    if (slot.lane === "ordinary") this.#growthFailed = true;
    if (!this.#closed) {
      recordCatalogCounter("trellis.auth.worker.failures", 1, {
        "trellis.kind": slot.lane,
        "trellis.phase": slot.started ? "worker.execute" : "worker.start",
      });
    }
    if (slot.started) {
      recordCatalogUpDown("trellis.auth.worker.active", -1, {
        "trellis.kind": slot.lane,
      });
    }
    slot.ready.reject(error);
    if (slot.active) {
      recordCatalogUpDown(
        "trellis.auth.worker.payload_bytes",
        -slot.copiedBytes,
        { "trellis.kind": slot.active.lane },
      );
      slot.copiedBytes = 0;
      slot.active.settled.reject(error);
      slot.active = undefined;
    }
    for (const task of slot.queue.splice(0)) task.settled.reject(error);
    for (const release of slot.releases.values()) release.done.reject(error);
    slot.installed.clear();
    void slot.worker.terminate();
  }
}
