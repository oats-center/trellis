import { deepEqual, equal } from "node:assert/strict";

import { test } from "node:test";
import { apis } from "trellis-web-generated";

import { UserCreationController } from "./user_creation.ts";

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
      principalId: userId,
      emailVerified: false,
      displayName: name ?? userId,
      account: {
        hasLocalPassword: false,
        profileRevision: 1n,
        gravatarEnabled: true,
        username: `user-${userId}`,
        principal: {
          principalId: userId,
          title: name ?? userId,
          enabled: true,
          kind: "user",
          revision: 1n,
          createdAt: "2026-01-01T00:00:00.000Z",
        },
      },
    },
  };
}

function setupReady(_userId: string, token: string): SetupOutput {
  return apis.auth.API.actions["rpc:Users.PasswordReset.Create"].output.decode({
    token,
    expiresAt: "2027-01-01T00:00:00.000Z",
  });
}

function denial(): unknown {
  return { code: "not_authorized", message: "not authorized" };
}

function ports(
  overrides: {
    createUser?: () => Promise<CreateOutput>;
    createSetupLink?: () => Promise<SetupOutput>;
  } = {},
) {
  return {
    createUser:
      overrides.createUser ??
      (() => Promise.resolve(createdUser("01ARZ3NDEKTSV4RRFFQ69G5FAV", "A"))),
    createSetupLink:
      overrides.createSetupLink ??
      (() =>
        Promise.resolve(
          setupReady("01ARZ3NDEKTSV4RRFFQ69G5FAV", "https://setup/a"),
        )),
    projectError: (error: unknown) => {
      const record = error as { code?: string; message?: string };
      return {
        message: record.message ?? "failed",
        ...(record.code === undefined ? {} : { code: record.code }),
      };
    },
  };
}

test("V03 disposal before create resolution commits nothing and dispatches no setup", async () => {
  const create = deferred<CreateOutput>();
  let setupDispatches = 0;
  const notifications: string[] = [];
  const controller = new UserCreationController(
    ports({
      createUser: () => create.promise,
      createSetupLink: () => {
        setupDispatches += 1;
        return Promise.resolve(
          setupReady("01ARZ3NDEKTSV4RRFFQ69G5FAV", "https://setup/a"),
        );
      },
    }),
    { onCreated: (user) => notifications.push(user.userId) },
  );

  const pending = controller.createUser({ username: "a", name: null });
  controller.dispose();
  create.resolve(createdUser("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
  await pending;

  equal(setupDispatches, 0, "a disposed workflow must not start setup");
  equal(notifications.length, 0, "a disposed workflow must not notify");
  equal(controller.snapshot.createdUser, null);
  equal(controller.snapshot.stage, "creatingUser");
});

test("V04 a late setup success after disposal exposes no receipt", async () => {
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

  const pending = controller.createUser({ username: "a", name: null });
  // Let the create resolve so setup is genuinely in flight, then end the page.
  while (controller.snapshot.stage === "creatingUser") {
    await Promise.resolve();
  }
  equal(controller.snapshot.stage, "creatingSetupLink");
  controller.dispose();
  setup.resolve(setupReady("01ARZ3NDEKTSV4RRFFQ69G5FAV", "https://setup/a"));
  await pending;

  equal(
    controller.snapshot.setupReceipt,
    null,
    "a disposed workflow must not expose a one-time receipt",
  );
  equal(notifications, 0, "a disposed workflow must not notify");
});

test("V06 a busy workflow cannot start another account", async () => {
  const create = deferred<CreateOutput>();
  const controller = new UserCreationController(
    ports({ createUser: () => create.promise }),
  );
  const pending = controller.createUser({ username: "a", name: null });
  equal(controller.snapshot.busy, true);
  equal(
    controller.startAnother(),
    false,
    "starting another account while a create is pending must be refused",
  );
  create.resolve(createdUser("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
  await pending;
});

test("V06 an unknown create outcome requires explicit reconciliation", () => {
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
  return controller.createUser({ username: "a", name: null }).then(() => {
    equal(controller.snapshot.stage, "createUnknown");
    equal(controller.snapshot.createdUser, null);
    equal(
      controller.startAnother(),
      false,
      "an unknown create outcome must not silently start another account",
    );
  });
});

test("V05 a denied setup leaves the account committed and retry available", async () => {
  let setupAttempts = 0;
  const controller = new UserCreationController(
    ports({
      createUser: () =>
        Promise.resolve(createdUser("01ARZ3NDEKTSV4RRFFQ69G5FAV", "A")),
      createSetupLink: () => {
        setupAttempts += 1;
        return setupAttempts === 1
          ? Promise.reject(denial())
          : Promise.resolve(
              setupReady("01ARZ3NDEKTSV4RRFFQ69G5FAV", "https://setup/a"),
            );
      },
    }),
  );

  await controller.createUser({ username: "a", name: null });
  equal(controller.snapshot.stage, "setupFailed");
  equal(controller.snapshot.setupDenied, true);
  equal(
    controller.snapshot.createdUser?.userId,
    "01ARZ3NDEKTSV4RRFFQ69G5FAV",
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
  equal(receipt?.userId, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
  equal(controller.snapshot.setupDenied, false);
});

test("V05 a retry never calls Users.Create again", async () => {
  let createDispatches = 0;
  const controller = new UserCreationController(
    ports({
      createUser: () => {
        createDispatches += 1;
        return Promise.resolve(createdUser("01ARZ3NDEKTSV4RRFFQ69G5FAV", "A"));
      },
      createSetupLink: () => Promise.reject(denial()),
    }),
  );
  await controller.createUser({ username: "a", name: null });
  await controller.createSetupLink();
  equal(createDispatches, 1, "setup retries must not re-create the account");
});

test("V06 an unknown setup outcome blocks action until reconciliation", async () => {
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
  await controller.createUser({ username: "a", name: null });
  equal(controller.snapshot.stage, "setupUnknown");
  equal(controller.snapshot.setupReceipt, null);
  equal(
    controller.startAnother(),
    false,
    "an unknown setup outcome must not start another account",
  );
});

test("V06 clearReceipt keeps the account and rebases the workflow", async () => {
  const controller = new UserCreationController(ports());
  await controller.createUser({ username: "a", name: null });
  const before = controller.snapshot.setupReceipt as {
    userId: string;
  } | null;
  equal(before?.userId, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
  controller.clearReceipt();
  equal(controller.snapshot.setupReceipt, null);
  equal(controller.snapshot.stage, "userCreated");
  equal(
    controller.snapshot.createdUser?.userId,
    "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    "the account identity survives clearing a one-time receipt",
  );
});

test("a blank username is rejected before any dispatch", async () => {
  let dispatches = 0;
  const controller = new UserCreationController(
    ports({
      createUser: () => {
        dispatches += 1;
        return Promise.resolve(createdUser("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
      },
    }),
  );
  await controller.createUser({ username: "   ", name: null });
  equal(dispatches, 0);
  equal(controller.snapshot.failure?.message, "Username is required.");
});

test("startAnother resets every workflow field", async () => {
  const controller = new UserCreationController(ports());
  await controller.createUser({ username: "a", name: null });
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
