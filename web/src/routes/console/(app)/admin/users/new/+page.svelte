<script lang="ts">
  import { type apis } from "trellis-web-generated";
  import { resolve, consoleUrl } from "$lib/console_paths";
  import {
    UserCreationController,
    type CreatedUser,
    type SetupReceipt,
    type UserCreationFailure,
    type UserCreationStage,
  } from "$lib/console/user_creation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { formatDate } from "$lib/format";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";
  import { onDestroy } from "svelte";

  const trellis = getTrellis();
  const notifications = getNotifications();

  function failureFrom(cause: unknown): UserCreationFailure {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  function trimmedOptional(value: string): string | null {
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : null;
  }

  let username = $state("");
  let name = $state("");
  let email = $state("");

  // Mirrors of the controller snapshot, kept as reactive page state. The
  // controller is the single owner; these are only its rendered projection.
  let stage = $state<UserCreationStage>("editing");
  let createdUser = $state.raw<CreatedUser | null>(null);
  let setupReceipt = $state.raw<SetupReceipt | null>(null);
  let failure = $state.raw<UserCreationFailure | null>(null);
  let setupDenied = $state(false);

  // The controller owns the real workflow: stages, captured intents, the
  // created account, the one-time receipt, and continuation ownership.
  const workflow = new UserCreationController(
    {
      createUser: async (input: apis.auth.UsersCreateInput) =>
        await trellis.usersCreate(input).orThrow(),
      createSetupLink: async (input: apis.auth.UsersPasswordResetCreateInput) =>
        await trellis.usersPasswordResetCreate(input).orThrow(),
      projectError: failureFrom,
    },
    {
      onCreated: (created) =>
        notifications.success(`Account ${created.label} created.`, "Created"),
      onSetupReady: () =>
        notifications.success("Password setup link created.", "Setup link"),
    },
    (snapshot) => {
      stage = snapshot.stage;
      createdUser = snapshot.createdUser;
      setupReceipt = snapshot.setupReceipt;
      failure = snapshot.failure;
      setupDenied = snapshot.setupDenied;
    },
  );
  onDestroy(() => workflow.dispose());

  const busy = $derived(stage === "creatingUser" || stage === "creatingSetupLink");
  const userExists = $derived(createdUser !== null);
  const setupPending = $derived(stage === "creatingSetupLink");
  const unknownOutcome = $derived(
    stage === "createUnknown" || stage === "setupUnknown",
  );

  async function createUser(): Promise<void> {
    await workflow.createUser({
      username,
      name: trimmedOptional(name),
      email: trimmedOptional(email),
    });
  }

  async function copySetupUrl(): Promise<void> {
    if (setupReceipt === null) return;
    if (typeof navigator === "undefined" || !navigator.clipboard) {
      notifications.error("Clipboard access is unavailable in this browser.", "Copy failed");
      return;
    }
    try {
      await navigator.clipboard.writeText(setupReceipt.setupUrl);
      notifications.success("Setup URL copied to clipboard.", "Copied");
    } catch (cause) {
      notifications.error(failureFrom(cause).message, "Copy failed");
    }
  }

  function startAnother(): void {
    if (!workflow.startAnother()) return;
    username = "";
    name = "";
    email = "";
  }
</script>

<section class="space-y-4">
  <PageToolbar title="New user" description="Create a local account and, as a separate step, a password setup link.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Back to users</a>
    {/snippet}
  </PageToolbar>

  {#if failure}
    <Notice variant={stage === "setupFailed" || stage === "setupUnknown" || stage === "createUnknown" ? "warning" : "error"}>
      {failure.message}
      {#if failure.id}<span class="ml-1 text-xs opacity-70">Reference: {failure.id}</span>{/if}
    </Notice>
  {/if}

  {#if userExists && createdUser}
    <section class="divide-y divide-base-300 border-y border-base-300 bg-base-100">
      <div class="px-5 py-3">
        <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">
          Account created
        </p>
        <p class="mt-1 text-sm font-medium">{createdUser.label}</p>
        <p class="trellis-identifier mt-1 break-all text-xs text-base-content/60">{createdUser.userId}</p>
        <p class="trellis-field-help mt-1">
          This account exists now. The password setup link is a separate operation.
        </p>
      </div>

      {#if stage === "setupReady" && setupReceipt}
        <div class="px-5 py-3">
          <div class="flex min-w-0 flex-wrap items-center justify-between gap-2">
            <div>
              <h2 class="text-sm font-semibold">Password setup URL</h2>
              <p class="trellis-field-help mt-1">
                Send this portal URL to the user to complete local password setup.
                It expires {formatDate(setupReceipt.expiresAt)}.
              </p>
            </div>
            <div class="flex flex-wrap gap-2">
              <button class="btn btn-outline btn-sm" type="button" onclick={copySetupUrl}>Copy setup URL</button>
              <a class="btn btn-ghost btn-sm" href={setupReceipt.setupUrl} target="_blank" rel="noreferrer">Open</a>
            </div>
          </div>
          <input class="input input-bordered input-sm mt-3 w-full trellis-identifier" readonly value={setupReceipt.setupUrl} aria-label="Password setup URL" />
          <p class="trellis-field-help mt-2">
            A lost setup link is not recoverable; creating another one is a new explicit operation.
          </p>
        </div>
      {:else if stage === "setupFailed" || stage === "setupUnknown" || stage === "creatingSetupLink"}
        <div class="px-5 py-3">
          <h2 class="text-sm font-semibold">
            {stage === "setupUnknown"
              ? "User created; setup-link outcome unknown"
              : stage === "creatingSetupLink"
              ? "Creating password setup link"
              : "User created; setup link unavailable"}
          </h2>
          <p class="trellis-field-help mt-1">
            {stage === "setupUnknown"
              ? "The request may have created a one-time link that cannot be recovered. Check the user before issuing another."
              : stage === "creatingSetupLink"
              ? "The account already exists; this step only creates its one-time password setup link."
              : setupDenied
              ? "Trellis denied the setup-link operation for this account. The user account remains created."
              : "The setup link operation failed. The user account remains created; retry only the setup link."}
          </p>
          <button class="btn btn-outline btn-sm mt-2" type="button" onclick={() => void workflow.createSetupLink()} disabled={stage === "setupUnknown" || setupPending}>
            {setupPending ? "Creating setup link…" : "Create setup link"}
          </button>
        </div>
      {/if}

      <div class="flex flex-wrap justify-end gap-2 px-5 py-3">
        <a class="btn btn-ghost btn-sm" href={consoleUrl("/admin/users/edit", { query: { userId: createdUser.userId } })}>
          Edit user
        </a>
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Back to users</a>
        <button class="btn btn-outline btn-sm" type="button" onclick={startAnother} disabled={busy || unknownOutcome}>Create another user</button>
      </div>
    </section>
  {:else}
    <form class="divide-y divide-base-300 border-y border-base-300 bg-base-100" onsubmit={(event) => { event.preventDefault(); void createUser(); }}>
      <section class="px-5 py-3">
        <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">User profile</p>
        <div class="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
          <label class="form-control w-full">
            <span class="label py-1"><span class="label-text text-xs">Username</span><span class="label-text-alt">required</span></span>
            <input class="input input-bordered input-sm trellis-identifier" bind:value={username} autocomplete="username" placeholder="local login" required />
          </label>
          <label class="form-control w-full">
            <span class="label py-1"><span class="label-text text-xs">Name</span><span class="label-text-alt">optional</span></span>
            <input class="input input-bordered input-sm" bind:value={name} autocomplete="name" placeholder="Operator name" />
          </label>
          <label class="form-control w-full md:col-span-2">
            <span class="label py-1"><span class="label-text text-xs">Email</span><span class="label-text-alt">optional</span></span>
            <input class="input input-bordered input-sm" type="email" bind:value={email} autocomplete="email" placeholder="user@example.com" />
          </label>
        </div>
        <p class="trellis-field-help mt-3">
          Creating the account and creating its password setup link are two separate
          operations. Participant capability policy is not set here; use Portal grants or
          User grants for real authority.
        </p>
      </section>

      <section class="flex flex-wrap justify-end gap-2 px-5 py-3">
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Cancel</a>
        <button class="btn btn-primary btn-sm" type="submit" disabled={busy || unknownOutcome || username.trim() === ""}>
          {stage === "creatingUser" ? "Creating…" : "Create user"}
        </button>
      </section>
    </form>
  {/if}
</section>
