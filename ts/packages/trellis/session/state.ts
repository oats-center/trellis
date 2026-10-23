import { AsyncResult, BaseError, Result } from "@oatscenter/result";
import { type RuntimeStateStores } from "./contract_types.ts";
import {
  API as STATE_API,
  Conflict as StateRpcConflict,
  type DeleteOutput as StateDeleteResponse,
  type GetOutput as StateGetResponse,
  type PutOutput as StateSetResponse,
  type RegisterInput as StateRegisterRequest,
  type RegisterOutput as StateRegisterResponse,
} from "../internal_sdk/generated/apis/state/mod.js";
import { type LiveSubscribeOpts } from "./types.ts";
import { type LiveSubscription } from "../live/subscription.ts";
import { type KvRepresentation, type ResourceMigrations } from "../kv.ts";
import { ulid } from "ulid";
import { codecs } from "../generated.ts";
import { UnexpectedError } from "../errors/index.ts";

/** Decoded current State value and its opaque storage revision. */
export type StateValue<T> = Readonly<{
  value: T;
  revision: string;
  createdAt: string;
  updatedAt: string;
}>;

/** State CAS failure with the authoritative current value, when present. */
export class StateConflictError<T> extends BaseError {
  override readonly name = "StateConflictError" as const;
  readonly current?: StateValue<T>;

  constructor(current?: StateValue<T>) {
    super("State revision conflict");
    this.current = current;
  }

  override toSerializable() {
    return this.baseSerializable();
  }
}

/** Raised when an optional participant resource is not installed. */
export class ResourceUnavailableError extends BaseError {
  override readonly name = "ResourceUnavailableError" as const;

  /** Construct an unavailable-resource failure. */
  constructor(
    readonly kind: string,
    readonly resourceName: string,
  ) {
    super(`${kind} resource '${resourceName}' is unavailable`);
  }

  override toSerializable() {
    return this.baseSerializable();
  }
}

/** Typed single-value State handle. */
export type ValueStateStoreClient<TValue> = StateHandle<TValue>;

export type StateFacade<TState extends RuntimeStateStores> = {
  [K in keyof TState]: ValueStateStoreClient<TState[K]["value"]>;
};

export const STATE_RUNTIME_RPC = {
  Register: {
    subject: "rpc.v1.state.Register",
    input: STATE_API.actions["rpc:Register"].input,
    output: STATE_API.actions["rpc:Register"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Register"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Register"].errors.map(
      (error) => error.type,
    ),
    runtimeErrors: STATE_API.actions["rpc:Register"].errors,
  },
  Get: {
    subject: "rpc.v1.state.Get",
    input: STATE_API.actions["rpc:Get"].input,
    output: STATE_API.actions["rpc:Get"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Get"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Get"].errors.map(
      (error) => error.type,
    ),
    runtimeErrors: STATE_API.actions["rpc:Get"].errors,
  },
  Put: {
    subject: "rpc.v1.state.Put",
    input: STATE_API.actions["rpc:Put"].input,
    output: STATE_API.actions["rpc:Put"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Put"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Put"].errors.map(
      (error) => error.type,
    ),
    runtimeErrors: STATE_API.actions["rpc:Put"].errors,
  },
  Delete: {
    subject: "rpc.v1.state.Delete",
    input: STATE_API.actions["rpc:Delete"].input,
    output: STATE_API.actions["rpc:Delete"].output,
    callerCapabilities: [],
    errors: STATE_API.actions["rpc:Delete"].errors.map((error) => error.type),
    declaredErrorTypes: STATE_API.actions["rpc:Delete"].errors.map(
      (error) => error.type,
    ),
    runtimeErrors: STATE_API.actions["rpc:Delete"].errors,
  },
};

/** Transport calls consumed by a typed State handle. */
export type StateResourceCalls = Readonly<{
  watch?(
    input: { resourceName: string },
    opts?: LiveSubscribeOpts,
  ): PromiseLike<Result<LiveSubscription<StateGetResponse>, BaseError>>;
  register?(
    input: StateRegisterRequest,
  ): PromiseLike<Result<StateRegisterResponse, BaseError>>;
  get(input: {
    resourceName: string;
  }): PromiseLike<Result<StateGetResponse, BaseError>>;
  set(input: {
    resourceName: string;
    representationVersion: bigint;
    value: Uint8Array;
    mode: "create" | "set" | "replace";
    revision?: string;
  }): PromiseLike<Result<StateSetResponse, BaseError>>;
  delete(input: {
    resourceName: string;
    revision?: string;
  }): PromiseLike<Result<StateDeleteResponse, BaseError>>;
}>;

/** Native-IDL projected schemas registered by an app before accessing State. */
export type StateRegistration = Readonly<{
  representation: StateRegisterRequest["representation"];
  accepts: readonly StateRegisterRequest["representation"][];
}>;

/** Owned certified Live observation of typed State snapshots; `undefined` means unwritten or deleted. */
export type StateWatch<T> = AsyncIterableIterator<StateValue<T> | undefined> &
  Pick<LiveSubscription<StateValue<T> | undefined>, "closed" | "close"> &
  AsyncDisposable;

/** Typed handle for one generated single-value State resource. */
export class StateHandle<T> {
  readonly #resourceName: string;
  readonly #definition: KvRepresentation<T>;
  readonly #migrations: ResourceMigrations<T>;
  readonly #isCurrent: () => boolean;
  readonly #isAvailable: () => boolean;
  readonly #calls: StateResourceCalls;
  readonly #registration?: StateRegistration;
  #registered?: Promise<Result<StateRegisterResponse, BaseError>>;
  #registrationRequest?: StateRegisterRequest;

  /** Construct a handle from generated metadata and settled State transport calls. */
  constructor(args: {
    resourceName: string;
    definition: KvRepresentation<T>;
    migrations?: ResourceMigrations<T>;
    isCurrent?: () => boolean;
    isAvailable?: () => boolean;
    calls: StateResourceCalls;
    registration?: StateRegistration;
  }) {
    this.#resourceName = args.resourceName;
    this.#definition = args.definition;
    this.#migrations = args.migrations ?? {};
    this.#isCurrent = args.isCurrent ?? (() => true);
    this.#isAvailable = args.isAvailable ?? (() => true);
    this.#calls = args.calls;
    this.#registration = args.registration;
  }

  /** Register an app representation with explicit CAS. Initial registration uses `0`; conflicts are never retried automatically. */
  register(
    expectedRevision = "0",
    idempotencyKey = ulid(),
  ): AsyncResult<StateRegisterResponse, BaseError> {
    this.#registrationRequest = undefined;
    return this.#sendRegistration((registration) => ({
      resourceName: this.#resourceName,
      representation: registration.representation,
      accepts: [...registration.accepts],
      expectedRevision,
      idempotencyKey: codecs.ulid.decode(idempotencyKey),
    }));
  }

  #sendRegistration(
    request: (registration: StateRegistration) => StateRegisterRequest,
  ): AsyncResult<StateRegisterResponse, BaseError> {
    const pending = (async () => {
      try {
        this.#assertCurrent();
        if (!this.#registration || !this.#calls.register)
          throw new Error("Only app State uses representation registration");
        const input = request(this.#registration);
        this.#registrationRequest = input;
        return await this.#calls.register(input);
      } catch (cause) {
        return Result.err(
          cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
        );
      }
    })().then((result) => {
      if (
        this.#registered === pending &&
        result.isErr() &&
        result.error.toSerializable().type !==
          "trellis.state@v1::RegistrationConflict"
      ) {
        this.#registered = undefined;
      }
      return result;
    });
    this.#registered = pending;
    return AsyncResult.from(pending);
  }

  async #ensureRegistered(): Promise<void> {
    if (!this.#registration) return;
    const request = this.#registrationRequest;
    (
      await (this.#registered ??
        (request ? this.#sendRegistration(() => request) : this.register()))
    ).orThrow();
  }

  /** Observe the initial snapshot and subsequent changes, migrating only in memory. Close or leave iteration to release the Live observation. */
  watch(opts?: LiveSubscribeOpts): AsyncResult<StateWatch<T>, BaseError> {
    return AsyncResult.from(
      (async () => {
        try {
          this.#assertCurrent();
          await this.#ensureRegistered();
          if (!this.#calls.watch)
            throw new Error("Generated State Watch transport is missing");
          const opened = await this.#calls.watch(
            { resourceName: this.#resourceName },
            opts,
          );
          if (opened.isErr()) return opened;
          const subscription = opened.orThrow();
          let pending = false;
          let ended = false;
          const observation: StateWatch<T> = {
            closed: subscription.closed,
            close: () => {
              ended = true;
              return subscription.close();
            },
            [Symbol.asyncIterator]() {
              return this;
            },
            next: async () => {
              if (ended) return { done: true, value: undefined };
              if (pending)
                throw new Error("concurrent next() is not supported");
              pending = true;
              try {
                this.#assertCurrent();
                const next = await subscription.next();
                if (next.done) return { done: true, value: undefined };
                const value = next.value.entry
                  ? await this.#project(next.value.entry)
                  : undefined;
                if (ended) return { done: true, value: undefined };
                this.#assertCurrent();
                return { done: false, value };
              } catch (cause) {
                ended = true;
                await subscription.return();
                throw cause;
              } finally {
                pending = false;
              }
            },
            return: async () => {
              ended = true;
              await subscription.return();
              return { done: true, value: undefined };
            },
            throw: async (cause?: unknown) => {
              ended = true;
              await subscription.return();
              throw cause;
            },
            [Symbol.asyncDispose]: async () => {
              ended = true;
              await subscription.return();
            },
          };
          return Result.ok(observation);
        } catch (cause) {
          return Result.err(
            cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
          );
        }
      })(),
    );
  }

  async #project(
    entry: NonNullable<StateGetResponse["entry"]>,
  ): Promise<StateValue<T>> {
    const version = Number(entry.representationVersion);
    const encoded = JSON.parse(new TextDecoder().decode(entry.value));
    let value: T;
    if (version === this.#definition.version) {
      value = this.#definition.codec.decode(encoded);
    } else {
      const historicCodec = this.#definition.migrations[version];
      const migrate = this.#migrations[version];
      if (!historicCodec || !migrate) {
        throw new Error(`Unsupported State representation version ${version}`);
      }
      const migrated = await migrate(historicCodec.decode(encoded));
      value = this.#definition.codec.decode(
        this.#definition.codec.encode(
          migrated instanceof Result ? migrated.orThrow() : migrated,
        ),
      );
    }
    return {
      value,
      revision: entry.revision,
      createdAt: entry.createdAt,
      updatedAt: entry.updatedAt,
    };
  }

  #assertCurrent(): void {
    if (!this.#isAvailable()) {
      throw new ResourceUnavailableError("State", this.#resourceName);
    }
    if (!this.#isCurrent()) {
      throw new Error(
        `State resource '${this.#resourceName}' has a stale generation`,
      );
    }
  }

  async #conflict(error: StateRpcConflict) {
    return Result.err(
      new StateConflictError(
        error.data.current
          ? await this.#project(error.data.current)
          : undefined,
      ),
    );
  }

  #write(
    value: T,
    mode: "create" | "set" | "replace",
    expectedRevision?: string,
  ): AsyncResult<StateValue<T>, BaseError> {
    return AsyncResult.from(
      (async () => {
        try {
          this.#assertCurrent();
          await this.#ensureRegistered();
          const result = await this.#calls.set({
            resourceName: this.#resourceName,
            representationVersion: BigInt(this.#definition.version),
            value: new TextEncoder().encode(
              JSON.stringify(this.#definition.codec.encode(value)),
            ),
            mode,
            ...(expectedRevision === undefined
              ? {}
              : { revision: expectedRevision }),
          });
          if (result.isErr()) {
            return result.error instanceof StateRpcConflict
              ? await this.#conflict(result.error)
              : result;
          }
          return Result.ok(
            await this.#project(
              result.unwrapOrElse(() => {
                throw new Error("State set unexpectedly failed");
              }).entry,
            ),
          );
        } catch (cause) {
          return Result.err(
            cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
          );
        }
      })(),
    );
  }

  /** Read the current value, migrating only in memory when required. */
  get(): AsyncResult<StateValue<T> | undefined, BaseError> {
    return AsyncResult.from(
      (async () => {
        try {
          this.#assertCurrent();
          await this.#ensureRegistered();
          const result = await this.#calls.get({
            resourceName: this.#resourceName,
          });
          if (result.isErr()) return result;
          const response = result.unwrapOrElse(() => {
            throw new Error("State get unexpectedly failed");
          });
          return Result.ok(
            response.entry ? await this.#project(response.entry) : undefined,
          );
        } catch (cause) {
          return Result.err(
            cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
          );
        }
      })(),
    );
  }

  /** Create the value only when no live value exists. */
  create(value: T): AsyncResult<StateValue<T>, BaseError> {
    return this.#write(value, "create");
  }

  /** Unconditionally replace the current value. */
  set(value: T): AsyncResult<StateValue<T>, BaseError> {
    return this.#write(value, "set");
  }

  /** Replace the value only while `revision` remains current. */
  replace(revision: string, value: T): AsyncResult<StateValue<T>, BaseError> {
    return this.#write(value, "replace", revision);
  }

  /** Delete unconditionally or only while `revision` remains current. */
  delete(revision?: string): AsyncResult<boolean, BaseError> {
    return AsyncResult.from(
      (async () => {
        try {
          this.#assertCurrent();
          await this.#ensureRegistered();
          const result = await this.#calls.delete({
            resourceName: this.#resourceName,
            ...(revision === undefined ? {} : { revision }),
          });
          if (result.isErr()) {
            return result.error instanceof StateRpcConflict
              ? await this.#conflict(result.error)
              : result;
          }
          return Result.ok(
            result.unwrapOrElse(() => {
              throw new Error("State delete unexpectedly failed");
            }).deleted,
          );
        } catch (cause) {
          return Result.err(
            cause instanceof BaseError ? cause : new UnexpectedError({ cause }),
          );
        }
      })(),
    );
  }
}
