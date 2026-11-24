import { renderUnicodeCompact } from "uqr";

import type { ConfigFieldDescriptor } from "../generated_support.ts";
import { API as AUTH_API } from "../internal_sdk/generated/apis/auth/mod.js";
import type {
  AuthAppConfigResponse,
  AuthOwnConfigResponse,
} from "../internal_sdk/generated/types/index.js";
import type { LoggerLike } from "../globals.ts";
import type { TransportGenerationManager } from "../transport/generations.ts";
import { Backoff } from "../backoff.ts";

/** What a settings watch needs from the logical connection's transport. */
type ConfigTransport = Pick<
  TransportGenerationManager,
  "acquireFor" | "currentGeneration" | "subscribePublication"
>;

/** One live change to a participant's settings. */
export type ConfigChange<TValues> = {
  /** Values before the change. */
  readonly previous: TValues;
  /** Values now in effect. */
  readonly current: TValues;
  /** Fields whose value changed. Restart-only fields never appear here. */
  readonly changed: readonly (keyof TValues & string)[];
};

/** Revisions behind the values in effect. */
export type ConfigRevision = {
  /** A service's or device's deployment revision. */
  readonly deployment?: number;
  /** An instance's override revision, when it has overrides. */
  readonly instance?: number;
  /** An app's revision. */
  readonly app?: number;
};

/** What a settings check is given besides the candidate values. */
export type ConfigCheckContext<TValues> = {
  /**
   * Refuse the candidate revision for `fields`, saying why. The check keeps
   * running; every rejection it makes is reported together.
   */
  reject(fields: readonly (keyof TValues & string)[], reason: string): void;
  /**
   * Fields whose candidate value differs from the value in effect. Empty
   * when a newly registered check reads the values already in effect.
   */
  readonly changed: readonly (keyof TValues & string)[];
};

/**
 * Validates candidate settings before they take effect. Call `reject` to
 * refuse fields; a throw refuses every changed field with the error's message.
 */
export type ConfigCheck<TValues> = (
  candidate: TValues,
  context: ConfigCheckContext<TValues>,
) => void | Promise<void>;

/** Options for {@link ParticipantConfig.derive}. */
export type DeriveOptions<T> = {
  /**
   * Called with a replaced value once its replacement has taken effect, and
   * with the value in effect when the participant stops or its connection
   * closes.
   */
  dispose?: (previous: T) => void | Promise<void>;
};

/** A value built from settings, rebuilt when the fields it reads change. */
export interface DerivedConfig<T> {
  /** The value built from the settings in effect. Read it at each use. */
  readonly current: T;
}

/**
 * Settings Trellis keeps for this service, device, or app, declared with
 * `config` in its contract and set by administrators through the CLI or Console.
 *
 * A live change takes effect in two steps. Checks and derive builders run
 * against the candidate values first; if any refuses, the revision is
 * rejected, `current` keeps the values accepted last, and no `onChange`
 * listener runs. Only an accepted revision changes `current` and reaches
 * `onChange`. Read `current`, or a derived value's `current`, at each use:
 * a copy taken once keeps the values it was taken from.
 */
export interface ParticipantConfig<TValues> {
  /**
   * Values in effect: the revision accepted last. A restart-only field keeps
   * the value loaded when the participant connected until it connects again.
   */
  readonly current: TValues;
  /** The revisions behind `current`. */
  readonly revision: ConfigRevision;
  /**
   * Run `listener` after a revision that changes a field that applies live
   * is accepted. A throw from it is logged; it never rejects the revision.
   * Returns a function that removes the listener.
   */
  onChange(listener: (change: ConfigChange<TValues>) => void): () => void;
  /**
   * Check every later revision before it takes effect, and the values in
   * effect now. Resolves once the values in effect pass. While they don't,
   * it waits: the participant prints which fields were refused and why with
   * a link to its settings, reports the rejection to Trellis, and resolves
   * when an administrator saves a revision that passes.
   */
  check(check: ConfigCheck<TValues>): Promise<void>;
  /**
   * Build a value from settings, such as a client that holds a key. It is
   * built now from the values in effect, and rebuilt during the check step
   * of a revision that changes one of `fields`. A builder that throws
   * rejects that revision for `fields` and keeps the old value. `dispose`
   * receives a replaced value after its replacement takes effect, and the
   * value in effect when the participant stops. Resolves once a value is
   * built, waiting as {@link check} does while it can't be; it rejects once
   * the participant has stopped.
   */
  derive<T>(
    fields: readonly (keyof TValues & string)[],
    build: (candidate: TValues) => T | Promise<T>,
    options?: DeriveOptions<T>,
  ): Promise<DerivedConfig<T>>;
}

/** Fields a participant declares, keyed by name. */
export type ConfigFields = Readonly<Record<string, ConfigFieldDescriptor>>;

/** One reading of the participant's settings from Auth. */
export type ConfigReading = {
  readonly values: Readonly<Record<string, unknown>>;
  readonly revision: ConfigRevision;
  /** Console page where an administrator changes them. */
  readonly configUrl?: string;
};

/** Reads the participant's own settings from Auth. */
export type ConfigReader = (atConnect: boolean) => Promise<ConfigReading>;

/** What the participant did with the revision it read last. */
export type ConfigOutcome = {
  readonly state: "applied" | "rejected";
  readonly revision: ConfigRevision;
  /** Fields its checks refused. */
  readonly fields: readonly string[];
  readonly reason?: string;
};

/** A service's or device's own reading, from `Config.Own.Get`. */
export function ownReading(response: AuthOwnConfigResponse): ConfigReading {
  return {
    ...(response.configUrl === undefined
      ? {}
      : { configUrl: response.configUrl }),
    values: response.values,
    revision: {
      ...(response.deploymentRevision === undefined
        ? {}
        : { deployment: response.deploymentRevision }),
      ...(response.instanceRevision === undefined
        ? {}
        : { instance: response.instanceRevision }),
    },
  };
}

/** Decoded setting values a generated participant declares. */
export type ParticipantConfigOf<TContract> = TContract extends {
  readonly __runtimeTypes?: { config: infer TConfig };
}
  ? TConfig
  : Record<string, never>;

/** Decoded setting values a generated app request declares. */
export type AppConfigOf<TRequest> = TRequest extends {
  readonly config: infer TFields;
}
  ? {
      readonly [K in keyof TFields]: TFields[K] extends {
        readonly codec: { decode(value: unknown): infer TValue };
      }
        ? TValue
        : never;
    }
  : Record<string, never>;

/**
 * Read an app's settings at connect and follow changes to its client's
 * settings until `closed` settles. App settings always have defaults, so an
 * app never waits.
 * @internal Used by the browser and native app connect paths.
 */
export async function connectAppConfig<TValues>(args: {
  fields: ConfigFields | undefined;
  clientId: string;
  read: () => Promise<AuthAppConfigResponse>;
  transport: ConfigTransport;
  closed: Promise<unknown>;
  log: LoggerLike;
  timeoutMs: number;
}): Promise<ParticipantConfig<TValues>> {
  const fields = args.fields ?? {};
  const read: ConfigReader = async () => {
    const response = await args.read();
    return {
      values: response.values,
      revision:
        response.revision === undefined ? {} : { app: response.revision },
    };
  };
  // An app without settings reads nothing: it may not hold `config.readApp`.
  const declared = Object.keys(fields).length > 0;
  const handle = new ConfigHandle<TValues>(
    fields,
    declared ? await read(true) : { values: {}, revision: {} },
    args.log,
  );
  if (declared)
    handle.watch({
      transport: args.transport,
      subject: AUTH_API.actions["event:Config.Changed"].subject,
      matches: (event) => event.clientId === args.clientId,
      read,
      timeoutMs: args.timeoutMs,
    });
  // What it derived goes with the connection.
  void args.closed.finally(() => handle.retire());
  return handle;
}

/** A value one `derive` keeps, and how it is rebuilt. */
type DeriveSlot = {
  readonly fields: readonly string[];
  readonly build: (candidate: never) => unknown;
  readonly dispose?: (previous: never) => void | Promise<void>;
  built: boolean;
  value: unknown;
};

/** One refusal of a candidate revision. */
type Rejection = { fields: string[]; reason: string };

/** Longest rejection reason Auth records. */
const REASON_LIMIT = 1024;

/**
 * Live handle over decoded settings.
 * @internal Constructed by the service and device connect paths.
 */
export class ConfigHandle<TValues> implements ParticipantConfig<TValues> {
  readonly #fields: ConfigFields;
  readonly #log: LoggerLike;
  readonly #participantId: string;
  readonly #report?: (outcome: ConfigOutcome) => Promise<void>;
  readonly #status?: (text: string) => void;
  // Startup ends when a service first serves, or when a device first reads
  // its settings. Until then an `applied` outcome is held back, so a check
  // registered during startup that refuses the starting values is the first
  // thing Auth hears; a rejection is reported at once.
  readonly #startsOnRead: boolean;
  #started = false;
  // Accepted values by field name; `current` presents them as `TValues`.
  #current: Record<string, unknown>;
  #revision: ConfigRevision;
  // The revision read last, accepted or not, and where it is changed.
  #latest: ConfigRevision;
  #configUrl?: string;
  readonly #listeners = new Set<(change: ConfigChange<TValues>) => void>();
  readonly #checks = new Set<ConfigCheck<TValues>>();
  readonly #derives = new Set<DeriveSlot>();
  // A registration refused the values in effect and waits for a revision
  // that passes, so every reading is checked, changed or not.
  #blocked = false;
  #accepted = Promise.withResolvers<void>();
  #outcome: ConfigOutcome;
  // The outcome Auth confirmed last, and a newer one still to report.
  #reported = "";
  #unreported: ConfigOutcome | undefined;
  #reporting = false;
  // Ends a report retry's wait early, for a newer outcome.
  #wakeReporting: (() => void) | undefined;
  #shown = "";
  readonly #outcomeListeners = new Set<() => void>();
  readonly #stop = new AbortController();
  // Readings, registrations and their checks run one at a time, in order.
  #refreshing: Promise<void> = Promise.resolve();
  #retired: Promise<void> | undefined;

  constructor(
    fields: ConfigFields,
    loaded: ConfigReading,
    log: LoggerLike,
    options: {
      participantId?: string;
      /** Tells Auth what became of each revision; apps have no one to tell. */
      report?: (outcome: ConfigOutcome) => Promise<void>;
      /** Reports what the participant waits for, such as to systemd. */
      status?: (text: string) => void;
      /** A device ends its startup the first time it reads its settings. */
      startsOnRead?: boolean;
    } = {},
  ) {
    this.#startsOnRead = options.startsOnRead ?? false;
    this.#fields = fields;
    this.#log = log;
    this.#participantId = options.participantId ?? "This participant";
    this.#report = options.report;
    this.#status = options.status;
    this.#current = decodeValues(fields, loaded.values);
    this.#revision = loaded.revision;
    this.#latest = loaded.revision;
    this.#configUrl = loaded.configUrl;
    this.#outcome = { state: "applied", revision: loaded.revision, fields: [] };
    this.#settle(this.#outcome);
  }

  get current(): TValues {
    if (this.#startsOnRead) this.started();
    return this.#current as TValues;
  }

  get revision(): ConfigRevision {
    if (this.#startsOnRead) this.started();
    return this.#revision;
  }

  /**
   * End startup: every check and derive registered so far has run, so the
   * outcome held back is reported now. Services call it when they first
   * serve. @internal
   */
  started(): void {
    if (this.#started) return;
    this.#started = true;
    this.#send(this.#outcome);
  }

  onChange(listener: (change: ConfigChange<TValues>) => void): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async check(check: ConfigCheck<TValues>): Promise<void> {
    const waiting = await this.#serialize(async () => {
      this.#stop.signal.throwIfAborted();
      this.#checks.add(check);
      const refused = await this.#run(this.#current, [], [check], []);
      return refused.rejections.length === 0
        ? undefined
        : this.#refuseInEffect(refused.rejections);
    });
    await waiting?.accepted;
  }

  async derive<T>(
    fields: readonly (keyof TValues & string)[],
    build: (candidate: TValues) => T | Promise<T>,
    options: DeriveOptions<T> = {},
  ): Promise<DerivedConfig<T>> {
    const slot: DeriveSlot = {
      fields: [...fields],
      build: build as (candidate: never) => unknown,
      ...(options.dispose
        ? { dispose: options.dispose as (previous: never) => void }
        : {}),
      built: false,
      value: undefined,
    };
    const waiting = await this.#serialize(async () => {
      this.#stop.signal.throwIfAborted();
      this.#derives.add(slot);
      const refused = await this.#run(this.#current, [], [], [slot]);
      if (this.#stop.signal.aborted) {
        // Built for a participant that stopped meanwhile: it never serves.
        this.#derives.delete(slot);
        for (const [built, value] of refused.built)
          await this.#dispose(built, value);
        this.#stop.signal.throwIfAborted();
      }
      if (refused.rejections.length > 0)
        return this.#refuseInEffect(refused.rejections);
      slot.value = refused.built.get(slot);
      slot.built = true;
      return undefined;
    });
    await waiting?.accepted;
    const read = () => {
      if (this.#startsOnRead) this.started();
    };
    return {
      get current(): T {
        read();
        return slot.value as T;
      },
    };
  }

  /**
   * The `settings` health check: degraded while the newest revision is
   * rejected, with the reason. @internal
   */
  healthCheck(): {
    status: "ok" | "degraded";
    summary?: string;
    info?: Record<string, string | string[]>;
  } {
    const outcome = this.#outcome;
    if (outcome.state === "applied") return { status: "ok" };
    return {
      status: "degraded",
      summary: `Settings rejected: ${describe(outcome)}`,
      info: { fields: [...outcome.fields] },
    };
  }

  /** Run `listener` whenever the outcome of a reading changes. @internal */
  onOutcome(listener: () => void): void {
    this.#outcomeListeners.add(listener);
  }

  #serialize<T>(work: () => Promise<T>): Promise<T> {
    const done = this.#refreshing.then(work);
    this.#refreshing = done.then(
      () => {},
      () => {},
    );
    return done;
  }

  /**
   * Run `checks` and then `derives` against `candidate`. A derive is rebuilt
   * only when it has no value yet or one of its fields changed. Builders are
   * skipped once a check refuses, so a refused revision builds nothing.
   */
  async #run(
    candidate: Record<string, unknown>,
    changed: readonly string[],
    checks: Iterable<ConfigCheck<TValues>>,
    derives: Iterable<DeriveSlot>,
  ): Promise<{ rejections: Rejection[]; built: Map<DeriveSlot, unknown> }> {
    const rejections: Rejection[] = [];
    const built = new Map<DeriveSlot, unknown>();
    const view = candidate as TValues;
    for (const check of checks) {
      const found: Rejection[] = [];
      try {
        await check(view, {
          changed: changed as readonly (keyof TValues & string)[],
          reject: (fields, reason) =>
            found.push({ fields: [...fields], reason }),
        });
      } catch (error) {
        found.push({ fields: [...changed], reason: messageOf(error) });
      }
      rejections.push(...found);
    }
    if (rejections.length > 0) return { rejections, built };
    for (const slot of derives) {
      if (slot.built && !slot.fields.some((name) => changed.includes(name)))
        continue;
      try {
        built.set(slot, await slot.build(view as never));
      } catch (error) {
        rejections.push({ fields: [...slot.fields], reason: messageOf(error) });
      }
    }
    if (rejections.length > 0) {
      // Values built for a refused revision never take effect.
      for (const [slot, value] of built) void this.#dispose(slot, value);
      built.clear();
    }
    return { rejections, built };
  }

  /**
   * A new registration refused the values in effect. Record it for the
   * revision they came from and return the acceptance to wait for. It is
   * wrapped, so the serialized step that returns it doesn't wait for it and
   * hold up the readings that bring a fix.
   */
  #refuseInEffect(rejections: Rejection[]): { accepted: Promise<void> } {
    this.#blocked = true;
    // A newer revision already refused stays the one reported.
    if (this.#latest === this.#revision)
      this.#settle(rejected(this.#fields, this.#revision, rejections));
    return { accepted: this.#accepted.promise };
  }

  /**
   * Check a newer reading and take it if it passes. Restart-only fields keep
   * their connect-time value, and a field whose value was removed keeps its
   * last one until reconnect, when the participant waits for it again.
   */
  async #take(next: ConfigReading): Promise<void> {
    const decoded = decodeValues(this.#fields, next.values);
    const previous = this.#current;
    const candidate: Record<string, unknown> = { ...previous };
    const changed: (keyof TValues & string)[] = [];
    for (const [name, field] of Object.entries(this.#fields)) {
      if (field.restart || !(name in decoded)) continue;
      if (!sameValue(decoded[name], previous[name])) {
        candidate[name] = decoded[name];
        changed.push(name as keyof TValues & string);
      }
    }
    this.#latest = next.revision;
    if (next.configUrl !== undefined) this.#configUrl = next.configUrl;
    if (changed.length === 0 && !this.#blocked) {
      this.#revision = next.revision;
      this.#settle({ state: "applied", revision: next.revision, fields: [] });
      return;
    }
    const { rejections, built } = await this.#run(
      candidate,
      changed,
      this.#checks,
      this.#derives,
    );
    if (this.#stop.signal.aborted) {
      // The participant stopped while this revision was built: it never
      // takes effect, and what was built for it goes now.
      for (const [slot, value] of built) await this.#dispose(slot, value);
      return;
    }
    if (rejections.length > 0) {
      this.#settle(rejected(this.#fields, next.revision, rejections));
      return;
    }
    this.#current = candidate;
    this.#revision = next.revision;
    this.#blocked = false;
    for (const [slot, value] of built) {
      const replaced = slot.built ? [slot.value] : [];
      slot.value = value;
      slot.built = true;
      for (const previous of replaced) void this.#dispose(slot, previous);
    }
    this.#settle({ state: "applied", revision: next.revision, fields: [] });
    const accepted = this.#accepted;
    this.#accepted = Promise.withResolvers<void>();
    accepted.resolve();
    if (changed.length === 0) return;
    const change = {
      previous: previous as TValues,
      current: candidate as TValues,
      changed,
    };
    for (const listener of this.#listeners) {
      try {
        const returned: unknown = listener(change);
        if (returned instanceof Promise)
          returned.catch((error: unknown) =>
            this.#log.error({ error }, "A config change listener threw"),
          );
      } catch (error) {
        this.#log.error({ error }, "A config change listener threw");
      }
    }
  }

  /** Dispose `value`; settles once its disposer has, never rejecting. */
  async #dispose(slot: DeriveSlot, value: unknown): Promise<void> {
    if (!slot.dispose) return;
    try {
      await slot.dispose(value as never);
    } catch (error) {
      this.#log.warn({ error }, "Disposing a derived setting value threw");
    }
  }

  /** Record what became of a reading, tell Auth, and say why on a refusal. */
  #settle(outcome: ConfigOutcome): void {
    this.#outcome = outcome;
    for (const listener of this.#outcomeListeners) listener();
    const key = JSON.stringify(outcome);
    if (outcome.state === "rejected" && this.#shown !== key) {
      this.#shown = key;
      const detail = describe(outcome);
      const next = this.#blocked
        ? "It waits for settings that pass."
        : "It keeps the settings it accepted last.";
      if (this.#blocked) this.#status?.(`Settings rejected: ${detail}`);
      this.#log.warn(
        {
          participant: this.#participantId,
          revision: outcome.revision,
          fields: outcome.fields,
          reason: outcome.reason,
          configUrl: this.#configUrl,
        },
        "Settings rejected",
      );
      console.error(
        this.#configUrl === undefined
          ? `\n${this.#participantId} rejected its settings: ${detail}\n${next}`
          : `\n${this.#participantId} rejected its settings: ${detail}\n${next}\nFix them at ${this.#configUrl}\nor scan:\n${renderUnicodeCompact(this.#configUrl)}`,
      );
    }
    this.#send(outcome);
  }

  /** Tell Auth what became of a reading, unless it heard it already. */
  #send(outcome: ConfigOutcome): void {
    if (!this.#report) return;
    if (outcome.state === "applied" && !this.#started) return;
    // An outcome Auth has supersedes one still being retried.
    this.#unreported =
      JSON.stringify(outcome) === this.#reported ? undefined : outcome;
    this.#wakeReporting?.();
    if (!this.#reporting) void this.#reportUnreported(this.#report);
  }

  /**
   * Report the newest outcome until Auth confirms it. A report that fails is
   * sent again after a backoff, or at once when a newer outcome replaces it,
   * so the status administrators see catches up without another save. Every
   * failure is retried: the only refusal Auth gives is for an instance that
   * is no longer active, whose connection ends with it.
   */
  async #reportUnreported(
    report: (outcome: ConfigOutcome) => Promise<void>,
  ): Promise<void> {
    this.#reporting = true;
    const backoff = new Backoff(250, 30_000);
    const signal = this.#stop.signal;
    // The first failure is logged, then at most one a minute until recovery.
    let loggedAt: number | undefined;
    let held = 0;
    try {
      while (this.#unreported !== undefined && !signal.aborted) {
        const outcome = this.#unreported;
        try {
          await report(outcome);
          this.#reported = JSON.stringify(outcome);
          if (this.#unreported === outcome) this.#unreported = undefined;
          backoff.succeeded();
          if (loggedAt !== undefined) {
            loggedAt = undefined;
            held = 0;
            this.#log.info(
              { participant: this.#participantId },
              "Settings reported again",
            );
          }
        } catch (error) {
          const delayMs = backoff.failed();
          if (loggedAt === undefined || Date.now() - loggedAt >= 60_000) {
            loggedAt = Date.now();
            this.#log.warn(
              {
                participant: this.#participantId,
                error,
                held,
                retryInMs: delayMs,
              },
              "Could not report what became of the settings; retrying",
            );
            held = 0;
          } else {
            held += 1;
          }
          await new Promise<void>((resolve) => {
            const done = () => {
              clearTimeout(timer);
              signal.removeEventListener("abort", done);
              this.#wakeReporting = undefined;
              resolve();
            };
            const timer = setTimeout(done, delayMs);
            signal.addEventListener("abort", done, { once: true });
            this.#wakeReporting = done;
          });
        }
      }
    } finally {
      this.#reporting = false;
    }
  }

  /**
   * Follow `Config.Changed` events `matches` accepts until stopped.
   * Each event only prompts a fresh authenticated read, so its payload is
   * never trusted for values. The subscription follows the published
   * transport generation: when a successor is published it moves there and
   * releases the old generation so that one can drain.
   * @internal
   */
  watch(args: {
    transport: ConfigTransport;
    subject: string;
    matches: (event: Readonly<Record<string, unknown>>) => boolean;
    read: ConfigReader;
    timeoutMs: number;
  }): void {
    const signal = this.#stop.signal;
    const refresh = (): Promise<void> =>
      this.#serialize(async () => {
        if (signal.aborted) return;
        try {
          await this.#take(await args.read(false));
        } catch (error) {
          if (!signal.aborted) {
            this.#log.warn({ error }, "Could not read changed settings");
          }
        }
      });
    void (async () => {
      while (!signal.aborted) {
        let lease:
          Awaited<ReturnType<ConfigTransport["acquireFor"]>> | undefined;
        // Registered before acquiring, so a publication made while acquiring
        // is still seen. The first call reports the publication as it stands.
        let initial = true;
        let published = false;
        let onPublished = (): void => {};
        const unpublish = args.transport.subscribePublication(() => {
          if (initial) {
            initial = false;
            return;
          }
          published = true;
          onPublished();
        });
        try {
          lease = await args.transport.acquireFor(
            { subscribe: [args.subject] },
            { signal, deadlineMs: Date.now() + args.timeoutMs },
          );
          const held = lease;
          const subscription = held.nc.subscribe(args.subject);
          const close = () => subscription.unsubscribe();
          signal.addEventListener("abort", close, { once: true });
          // A successor's publication ends this subscription. The loop then
          // subscribes on the successor and reads again, and releasing this
          // lease lets the old generation drain.
          onPublished = () => {
            const current = args.transport.currentGeneration();
            if (current !== undefined && current.nc !== held.nc) close();
          };
          if (published) onPublished();
          await lease.nc.flush();
          // Changes made while no subscription existed are read now.
          await refresh();
          for await (const message of subscription) {
            let event: unknown;
            try {
              event = message.json();
            } catch {
              continue;
            }
            if (
              typeof event === "object" &&
              event !== null &&
              args.matches(event as Record<string, unknown>)
            )
              await refresh();
          }
          signal.removeEventListener("abort", close);
        } catch (error) {
          if (signal.aborted) break;
          this.#log.warn({ error }, "Settings watch interrupted; retrying");
          await new Promise<void>((resolve) => {
            const timer = setTimeout(resolve, 1_000);
            signal.addEventListener(
              "abort",
              () => {
                clearTimeout(timer);
                resolve();
              },
              { once: true },
            );
          });
        } finally {
          unpublish();
          lease?.release();
        }
      }
    })();
  }

  /**
   * Stop following changes. A registration still waiting rejects, and one
   * made from now on is refused. @internal
   */
  stop(): void {
    if (this.#stop.signal.aborted) return;
    this.#stop.abort(new Error("The participant stopped"));
    // A registration still waiting for settings that pass never gets them.
    const accepted = this.#accepted;
    void accepted.promise.catch(() => {});
    accepted.reject(new Error("The participant stopped"));
  }

  /**
   * Stop, and once the reading or registration in progress has finished,
   * dispose every derived value still in effect, so a participant that has
   * stopped leaves none of its clients, pools or timers running. A value
   * built for a revision that was in progress never takes effect and is
   * disposed too. Settles once every disposer has. @internal
   */
  retire(): Promise<void> {
    this.stop();
    this.#retired ??= this.#serialize(async () => {
      const retiring = [...this.#derives];
      this.#derives.clear();
      this.#checks.clear();
      this.#listeners.clear();
      for (const slot of retiring) {
        if (slot.built) await this.#dispose(slot, slot.value);
      }
    });
    return this.#retired;
  }
}

/** An outcome refusing `revision` for every field `rejections` name. */
function rejected(
  fields: ConfigFields,
  revision: ConfigRevision,
  rejections: readonly Rejection[],
): ConfigOutcome {
  const names = [
    ...new Set(
      rejections.flatMap((rejection) =>
        rejection.fields.filter((name) => name in fields),
      ),
    ),
  ];
  const reason = [...new Set(rejections.map(({ reason }) => reason))].join(
    "; ",
  );
  return {
    state: "rejected",
    revision,
    fields: names,
    reason:
      reason.length > REASON_LIMIT ? reason.slice(0, REASON_LIMIT) : reason,
  };
}

/** `awsKeyId, awsSecret: set both or neither`. */
function describe(outcome: ConfigOutcome): string {
  const reason = outcome.reason ?? "rejected";
  return outcome.fields.length === 0
    ? reason
    : `${outcome.fields.join(", ")}: ${reason}`;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Read the participant's settings at connect. While a required value is
 * missing, print where an administrator sets it and wait; never start with a
 * partial configuration.
 * @internal
 */
export async function loadConfig<
  TValues extends Record<string, unknown>,
>(args: {
  participantId: string;
  fields: ConfigFields;
  read: (atConnect: boolean) => Promise<AuthOwnConfigResponse>;
  /** Tells Auth whether each revision read was applied or rejected. */
  report: (outcome: ConfigOutcome) => Promise<void>;
  log: LoggerLike;
  /** Reports what the participant waits for, such as to systemd. */
  status?: (text: string) => void;
  /** A device ends its startup the first time it reads its settings. */
  startsOnRead?: boolean;
  signal?: AbortSignal;
}): Promise<ConfigHandle<TValues>> {
  const signal = args.signal;
  // Cancellation settles a pending read or wait at once; a read that finishes
  // after cancellation is discarded.
  const aborted = Promise.withResolvers<never>();
  void aborted.promise.catch(() => {});
  const abort = () => aborted.reject(signal?.reason);
  signal?.addEventListener("abort", abort, { once: true });
  try {
    const backoff = new Backoff(1_000, 15_000);
    let shown = "";
    for (;;) {
      signal?.throwIfAborted();
      const loaded = await Promise.race([args.read(true), aborted.promise]);
      signal?.throwIfAborted();
      if (loaded.missing.length === 0) {
        return new ConfigHandle<TValues>(
          args.fields,
          ownReading(loaded),
          args.log,
          {
            participantId: args.participantId,
            report: args.report,
            ...(args.status ? { status: args.status } : {}),
            ...(args.startsOnRead ? { startsOnRead: true } : {}),
          },
        );
      }
      const missing = loaded.missing.join(", ");
      // A saved value that doesn't fit this build counts as missing; say why.
      const reasons = loaded.missing.flatMap((name) => {
        const reason = loaded.invalid[name];
        return reason === undefined ? [] : [reason];
      });
      const reported = `${missing}${reasons.map((reason) => `\n  ${reason}`).join("")}`;
      if (shown !== reported) {
        shown = reported;
        args.status?.(`Waiting for settings: ${missing}`);
        args.log.warn(
          {
            participant: args.participantId,
            missing: loaded.missing,
            invalid: reasons,
            configUrl: loaded.configUrl,
          },
          "Waiting for an administrator to set required settings",
        );
        console.error(
          loaded.configUrl === undefined
            ? `\n${args.participantId} needs these settings before it starts: ${reported}`
            : `\n${args.participantId} needs these settings before it starts: ${reported}\nSet them at ${loaded.configUrl}\nor scan:\n${renderUnicodeCompact(loaded.configUrl)}`,
        );
      }
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await Promise.race([
          new Promise<void>((resolve) => {
            timer = setTimeout(resolve, backoff.failed());
          }),
          aborted.promise,
        ]);
      } finally {
        clearTimeout(timer);
      }
    }
  } finally {
    signal?.removeEventListener("abort", abort);
  }
}

function decodeValues(
  fields: ConfigFields,
  values: Readonly<Record<string, unknown>>,
): Record<string, unknown> {
  const decoded: Record<string, unknown> = {};
  for (const [name, field] of Object.entries(fields)) {
    if (name in values) decoded[name] = field.codec.decode(values[name]);
  }
  return decoded;
}

function sameValue(left: unknown, right: unknown): boolean {
  return (
    JSON.stringify(left, (_key, value) =>
      typeof value === "bigint" ? `${value}n` : value,
    ) ===
    JSON.stringify(right, (_key, value) =>
      typeof value === "bigint" ? `${value}n` : value,
    )
  );
}
