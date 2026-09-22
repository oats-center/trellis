/**
 * Production two-stage account-creation workflow.
 *
 * `Users.Create` and `Users.PasswordReset.Create` are separate operations with
 * separate authority, so a failed or denied setup step must never suggest
 * creating the account again. This controller owns the whole state machine,
 * including which continuation may still commit.
 *
 * It performs no I/O and uses no Svelte state: the page passes the real
 * generated-client calls in, and the controller decides what is allowed to
 * mutate observable state. Two rules drive every transition:
 *
 * - a disposed controller or a superseded workflow generation commits nothing
 *   and dispatches nothing further, even when an in-flight request succeeds;
 * - a definite Trellis denial is a *setup-step* failure, while the account
 *   stays committed and an explicit retry remains available.
 */

import type { apis } from "trellis-web-generated";
import { ulid } from "ulid";

import {
  captureIntent,
  MutationController,
  type MutationOutcome,
} from "./mutation.ts";

/** Input for the account-creating operation. */
export type UserCreationCreateInput = apis.auth.UsersCreateInput;
/** Input for the password-setup-link operation. */
export type UserCreationSetupInput = apis.auth.UsersPasswordResetCreateInput;

/** Account identity retained once `Users.Create` succeeds. */
export type CreatedUser = {
  readonly userId: string;
  readonly label: string;
};

/** A one-time password setup URL bound to the account it was created for. */
export type SetupReceipt = {
  readonly userId: string;
  readonly label: string;
  readonly setupUrl: string;
  readonly expiresAt: bigint;
};

/** Safe, serializable failure detail for rendering. */
export type UserCreationFailure = {
  readonly message: string;
  readonly code?: string;
  readonly id?: string;
};

/** Observable stage of the two-stage workflow. */
export type UserCreationStage =
  | "editing"
  | "creatingUser"
  | "userCreated"
  | "creatingSetupLink"
  | "setupReady"
  | "setupFailed"
  | "createUnknown"
  | "setupUnknown";

/** Outcome of one dispatched operation as the controller records it. */
export type UserCreationDispatchOutcome<TOutput> = MutationOutcome<TOutput>;

/** Real generated-client boundary the page supplies. */
export type UserCreationPorts = {
  readonly createUser: (
    input: apis.auth.UsersCreateInput,
  ) => Promise<apis.auth.UsersCreateOutput>;
  readonly createSetupLink: (
    input: apis.auth.UsersPasswordResetCreateInput,
  ) => Promise<apis.auth.UsersPasswordResetCreateOutput>;
  /** Projects any thrown/returned failure into renderable copy. */
  readonly projectError: (error: unknown) => UserCreationFailure;
};

/** Immutable view of the controller for the page and its tests. */
export type UserCreationSnapshot = {
  readonly stage: UserCreationStage;
  readonly createdUser: CreatedUser | null;
  readonly setupReceipt: SetupReceipt | null;
  readonly failure: UserCreationFailure | null;
  /** True while either operation is in flight. */
  readonly busy: boolean;
  /** True when the last setup failure was an authoritative denial. */
  readonly setupDenied: boolean;
};

/** Callbacks the page uses to surface notifications. */
export type UserCreationObservers = {
  readonly onCreated?: (user: CreatedUser) => void;
  readonly onSetupReady?: (receipt: SetupReceipt) => void;
  readonly onSetupDenied?: (failure: UserCreationFailure) => void;
};

type State = UserCreationSnapshot;

const EMPTY: State = {
  stage: "editing",
  createdUser: null,
  setupReceipt: null,
  failure: null,
  busy: false,
  setupDenied: false,
};

/**
 * Owns one account-creation workflow for one page instance.
 *
 * The page must call `dispose()` from its teardown so a late success cannot
 * expose a one-time URL or dispatch a follow-up operation from a page the
 * operator already left.
 */
export class UserCreationController {
  #ports: UserCreationPorts;
  #observers: UserCreationObservers;
  #onChange?: (snapshot: UserCreationSnapshot) => void;
  #state: State = EMPTY;
  #generation = 0;
  #disposed = false;
  readonly #createMutation: MutationController<
    apis.auth.UsersCreateInput,
    apis.auth.UsersCreateOutput
  >;
  readonly #setupMutation: MutationController<
    apis.auth.UsersPasswordResetCreateInput,
    apis.auth.UsersPasswordResetCreateOutput
  >;

  constructor(
    ports: UserCreationPorts,
    observers: UserCreationObservers = {},
    onChange?: (snapshot: UserCreationSnapshot) => void,
  ) {
    this.#ports = ports;
    this.#observers = observers;
    this.#onChange = onChange;
    this.#createMutation = new MutationController();
    this.#setupMutation = new MutationController();
  }

  /** Current immutable snapshot. */
  get snapshot(): UserCreationSnapshot {
    return this.#state;
  }

  /** True once `dispose()` ran; the workflow can never commit again. */
  get disposed(): boolean {
    return this.#disposed;
  }

  /** The generation the live page owns; a stale one can never commit. */
  get generation(): number {
    return this.#generation;
  }

  #commit(patch: Partial<State>): void {
    this.#state = { ...this.#state, ...patch };
    this.#onChange?.(this.#state);
  }

  /**
   * Sends `Users.Create` once for the captured input and retains the returned
   * account identity. The returned value reports whether a setup request was
   * started, so the caller can await the whole workflow in tests.
   */
  async createUser(input: {
    readonly username: string;
    readonly name: string | null;
    readonly email: string | null;
  }): Promise<void> {
    if (this.#disposed || this.#state.stage !== "editing") return;
    const username = input.username.trim();
    if (username === "") {
      this.#commit({ failure: { message: "Username is required." } });
      return;
    }
    const generation = this.#generation;
    const key = ulid();
    const intent = captureIntent<apis.auth.UsersCreateInput>({
      operation: "usersCreate",
      targetId: username,
      label: username,
      idempotencyKey: key,
      input: {
        email: input.email,
        idempotencyKey: key,
        image: null,
        name: input.name,
        username,
      },
      scope: { routeKey: "user-new", workflowGeneration: generation },
    });
    if (!this.#createMutation.begin(intent)) return;
    this.#commit({ stage: "creatingUser", failure: null, busy: true });

    const outcome = await this.#createMutation.send({
      isStillValid: () => !this.#disposed && this.#generation === generation,
      dispatch: async ({ input: exact }) => await this.#ports.createUser(exact),
    });
    if (this.#disposed || this.#generation !== generation) return;
    if (outcome === null) {
      this.#commit({ stage: "editing", busy: false });
      return;
    }
    if (outcome.kind === "succeeded") {
      // The account exists from this point on. A later setup failure must never
      // re-create it.
      const created: CreatedUser = {
        userId: outcome.value.user.userId,
        label: outcome.value.user.name ??
          outcome.value.user.email ??
          outcome.value.user.userId,
      };
      this.#commit({
        stage: "userCreated",
        createdUser: created,
        busy: false,
        failure: null,
      });
      this.#observers.onCreated?.(created);
      await this.createSetupLink();
      return;
    }
    if (outcome.kind === "unknown") {
      this.#commit({
        stage: "createUnknown",
        busy: false,
        failure: {
          message:
            "Account creation outcome unknown. Check Users for this username before creating another account.",
        },
      });
      return;
    }
    this.#commit({
      stage: "editing",
      busy: false,
      failure: this.#ports.projectError(outcome.error),
    });
  }

  /**
   * Sends `Users.PasswordReset.Create` for the already-created account.
   *
   * This is the only way to recover the setup step: it never calls
   * `Users.Create` again, and a definite denial leaves the account committed
   * with the action still available.
   */
  async createSetupLink(): Promise<void> {
    const created = this.#state.createdUser;
    if (
      this.#disposed || created === null || this.#state.busy ||
      this.#state.stage === "setupUnknown"
    ) {
      return;
    }
    const userId = created.userId;
    const generation = this.#generation;
    const key = ulid();
    const intent = captureIntent<apis.auth.UsersPasswordResetCreateInput>({
      operation: "usersPasswordResetCreate",
      targetId: userId,
      label: created.label,
      idempotencyKey: key,
      input: { idempotencyKey: key, returnTarget: null, userId },
      scope: { routeKey: "user-new", workflowGeneration: generation },
    });
    if (!this.#setupMutation.begin(intent)) return;
    this.#commit({
      stage: "creatingSetupLink",
      failure: null,
      busy: true,
      setupDenied: false,
    });

    const outcome = await this.#setupMutation.send({
      isStillValid: () =>
        !this.#disposed && this.#generation === generation &&
        this.#state.createdUser?.userId === userId,
      dispatch: async ({ input: exact }) =>
        await this.#ports.createSetupLink(exact),
    });
    if (
      this.#disposed || this.#generation !== generation ||
      this.#state.createdUser?.userId !== userId
    ) {
      return;
    }
    if (outcome === null) {
      this.#commit({ stage: "userCreated", busy: false });
      return;
    }
    if (outcome.kind === "succeeded") {
      const receipt: SetupReceipt = {
        userId,
        label: created.label,
        setupUrl: outcome.value.flow.completionUrl,
        expiresAt: outcome.value.flow.expiresAt,
      };
      this.#commit({
        stage: "setupReady",
        setupReceipt: receipt,
        busy: false,
        failure: null,
      });
      this.#observers.onSetupReady?.(receipt);
      return;
    }
    if (outcome.kind === "unknown") {
      this.#commit({
        stage: "setupUnknown",
        busy: false,
        failure: {
          message:
            "Setup-link outcome unknown. Check the user's setup state before requesting a new link.",
        },
      });
      return;
    }
    const failure = this.#ports.projectError(outcome.error);
    this.#commit({
      stage: "setupFailed",
      busy: false,
      failure,
      setupDenied: failure.code === "not_authorized",
    });
    if (failure.code === "not_authorized") {
      this.#observers.onSetupDenied?.(failure);
    }
  }

  /**
   * Clears the one-time receipt. The account and its committed identity stay,
   * so a further setup link remains an explicit new operation for the same
   * user rather than a new account.
   */
  clearReceipt(): void {
    if (this.#disposed || this.#state.busy) return;
    this.#commit({ setupReceipt: null, stage: "userCreated" });
  }

  /**
   * Starts a new account workflow. Blocked while an operation is pending and
   * after an unknown outcome, which requires explicit reconciliation first.
   */
  startAnother(): boolean {
    if (
      this.#disposed || this.#state.busy ||
      this.#state.stage === "createUnknown" ||
      this.#state.stage === "setupUnknown"
    ) {
      return false;
    }
    this.#generation += 1;
    this.#createMutation.reset();
    this.#setupMutation.reset();
    this.#commit({ ...EMPTY });
    return true;
  }

  /**
   * Ends the workflow permanently.
   *
   * An in-flight request may still complete on the server, but this controller
   * commits nothing further: no receipt, no notification, no follow-up
   * mutation.
   */
  dispose(): void {
    this.#disposed = true;
    this.#generation += 1;
  }
}
