import { deepEqual, equal } from "node:assert/strict";

import type { apis } from "trellis-web-generated";

import { UserCreationController } from "./user_creation.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

type CreateOutput = apis.auth.UsersCreateOutput;
type SetupOutput = apis.auth.UsersPasswordResetCreateOutput;

/** Deferred promise standing in for one real generated-client call. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function createdUser(userId: string, name: string | null = null): CreateOutput {
  return {
    user: {
      userId,
      name,
      email: null,
      username: `user-${userId}`,
    },
  } as unknown as CreateOutput;
}

function setupReady(userId: string, url: string): SetupOutput {
  return {
    flow: {
      completionUrl: url,
      expiresAt: 1_800_000_000n,
      userId,
    },
  } as unknown as SetupOutput;
}

function denial(): unknown {
  return { code: "not_authorized", message: "not authorized" };
}

function ports(overrides: {
  createUser?: () => Promise<CreateOutput>;
  createSetupLink?: () => Promise<SetupOutput>;
} = {}) {
  return {
    createUser: overrides.createUser ??
      (() => Promise.resolve(createdUser("usr_a", "A"))),
    createSetupLink: overrides.createSetupLink ??
      (() => Promise.resolve(setupReady("usr_a", "https://setup/a"))),
    projectError: (error: unknown) => {
      const record = error as { code?: string; message?: string };
      return {
        message: record.message ?? "failed",
        ...(record.code === undefined ? {} : { code: record.code }),
      };
    },
  };
}

Deno.test("V03 disposal before create resolution commits nothing and dispatches no setup", async () => {
  const create = deferred<CreateOutput>();
  let setupDispatches = 0;
  const notifications: string[] = [];
  const controller = new UserCreationController(
    ports({
      createUser: () => create.promise,
      createSetupLink: () => {
        setupDispatches += 1;
        return Promise.resolve(setupReady("usr_a", "https://setup/a"));
      },
    }),
    { onCreated: (user) => notifications.push(user.userId) },
  );

  const pending = controller.createUser({
    username: "a",
    name: null,
    email: null,
  });
  controller.dispose();
  create.resolve(createdUser("usr_a"));
  await pending;

  equal(setupDispatches, 0, "a disposed workflow must not start setup");
  equal(notifications.length, 0, "a disposed workflow must not notify");
  equal(controller.snapshot.createdUser, null);
  equal(controller.snapshot.stage, "creatingUser");
});

Deno.test("V04 a late setup success after disposal exposes no receipt", async () => {
  const setup = deferred<SetupOutput>();
  let notifications = 0;
  const controller = new UserCreationController(
    ports({ createSetupLink: () => setup.promise }),
    {
      onSetupReady: () => {
        notifications += 1;
      },
    },
  );

  const pending = controller.createUser({
    username: "a",
    name: null,
    email: null,
  });
  // Let the create resolve so setup is genuinely in flight, then end the page.
  while (controller.snapshot.stage === "creatingUser") {
    await Promise.resolve();
  }
  equal(controller.snapshot.stage, "creatingSetupLink");
  controller.dispose();
  setup.resolve(setupReady("usr_a", "https://setup/a"));
  await pending;

  equal(
    controller.snapshot.setupReceipt,
    null,
    "a disposed workflow must not expose a one-time receipt",
  );
  equal(notifications, 0, "a disposed workflow must not notify");
});

Deno.test("V06 a busy workflow cannot start another account", async () => {
  const create = deferred<CreateOutput>();
  const controller = new UserCreationController(
    ports({ createUser: () => create.promise }),
  );
  const pending = controller.createUser({
    username: "a",
    name: null,
    email: null,
  });
  equal(controller.snapshot.busy, true);
  equal(
    controller.startAnother(),
    false,
    "starting another account while a create is pending must be refused",
  );
  create.resolve(createdUser("usr_a"));
  await pending;
});

Deno.test("V06 an unknown create outcome requires explicit reconciliation", () => {
  const controller = new UserCreationController(
    ports({
      createUser: () =>
        Promise.reject(
          Object.assign(new Error("socket closed"), {
            name: "TransportError",
          }),
        ),
    }),
  );
  return controller.createUser({ username: "a", name: null, email: null })
    .then(() => {
      equal(controller.snapshot.stage, "createUnknown");
      equal(controller.snapshot.createdUser, null);
      equal(
        controller.startAnother(),
        false,
        "an unknown create outcome must not silently start another account",
      );
    });
});

Deno.test("V05 a denied setup leaves the account committed and retry available", async () => {
  let setupAttempts = 0;
  const controller = new UserCreationController(
    ports({
      createUser: () => Promise.resolve(createdUser("usr_a", "A")),
      createSetupLink: () => {
        setupAttempts += 1;
        return setupAttempts === 1
          ? Promise.reject(denial())
          : Promise.resolve(setupReady("usr_a", "https://setup/a"));
      },
    }),
  );

  await controller.createUser({ username: "a", name: null, email: null });
  equal(controller.snapshot.stage, "setupFailed");
  equal(controller.snapshot.setupDenied, true);
  equal(
    controller.snapshot.createdUser?.userId,
    "usr_a",
    "the account stays committed after a denied setup step",
  );
  equal(
    controller.snapshot.setupReceipt,
    null,
    "a denial must not produce a receipt",
  );

  // An explicit retry sends setup again for the same user and never a second
  // create.
  await controller.createSetupLink();
  equal(setupAttempts, 2);
  equal(controller.snapshot.stage, "setupReady");
  const receipt = controller.snapshot.setupReceipt as {
    userId: string;
  } | null;
  equal(receipt?.userId, "usr_a");
  equal(controller.snapshot.setupDenied, false);
});

Deno.test("V05 a retry never calls Users.Create again", async () => {
  let createDispatches = 0;
  const controller = new UserCreationController(
    ports({
      createUser: () => {
        createDispatches += 1;
        return Promise.resolve(createdUser("usr_a", "A"));
      },
      createSetupLink: () => Promise.reject(denial()),
    }),
  );
  await controller.createUser({ username: "a", name: null, email: null });
  await controller.createSetupLink();
  equal(createDispatches, 1, "setup retries must not re-create the account");
});

Deno.test("V06 an unknown setup outcome blocks action until reconciliation", async () => {
  const controller = new UserCreationController(
    ports({
      createSetupLink: () =>
        Promise.reject(
          Object.assign(new Error("timeout"), {
            name: "TransportError",
            code: "timeout",
          }),
        ),
    }),
  );
  await controller.createUser({ username: "a", name: null, email: null });
  equal(controller.snapshot.stage, "setupUnknown");
  equal(controller.snapshot.setupReceipt, null);
  equal(
    controller.startAnother(),
    false,
    "an unknown setup outcome must not start another account",
  );
});

Deno.test("V06 clearReceipt keeps the account and rebases the workflow", async () => {
  const controller = new UserCreationController(ports());
  await controller.createUser({ username: "a", name: null, email: null });
  const before = controller.snapshot.setupReceipt as {
    userId: string;
  } | null;
  equal(before?.userId, "usr_a");
  controller.clearReceipt();
  equal(controller.snapshot.setupReceipt, null);
  equal(controller.snapshot.stage, "userCreated");
  equal(
    controller.snapshot.createdUser?.userId,
    "usr_a",
    "the account identity survives clearing a one-time receipt",
  );
});

Deno.test("a blank username is rejected before any dispatch", async () => {
  let dispatches = 0;
  const controller = new UserCreationController(
    ports({
      createUser: () => {
        dispatches += 1;
        return Promise.resolve(createdUser("usr_a"));
      },
    }),
  );
  await controller.createUser({ username: "   ", name: null, email: null });
  equal(dispatches, 0);
  equal(controller.snapshot.failure?.message, "Username is required.");
});

Deno.test("startAnother resets every workflow field", async () => {
  const controller = new UserCreationController(ports());
  await controller.createUser({ username: "a", name: null, email: null });
  equal(controller.startAnother(), true);
  deepEqual(controller.snapshot, {
    stage: "editing",
    createdUser: null,
    setupReceipt: null,
    failure: null,
    busy: false,
    setupDenied: false,
  });
});
