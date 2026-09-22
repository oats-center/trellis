<script lang="ts">
  import { browser } from "$app/environment";
  import { onMount } from "svelte";
  import { portalRedirectLocation } from "@qlever-llc/trellis/auth/browser";
  import { trellisUrl } from "$lib/portal_config";
  import PortalBrand from "$lib/components/PortalBrand.svelte";
  import { createLoginPortalFlow } from "$lib/portal_login";
  import {
    capabilityEntries,
    capabilityMetadata,
    portalCapabilityDisplayName,
    portalReturnLocation,
    isFederatedRegistrationAvailable,
    isFederatedRegistrationProvider,
    isLocalRegistrationAvailable,
    submitLocalLogin,
    submitLocalRegistration,
    shouldOfferPortalReturnLink,
    shouldShowPortalExpiredState,
    shouldStayOnPortalCompletionPage,
    visiblePortalFlowError,
  } from "./page_state";

  const flow = createLoginPortalFlow(pageUrl);
  let denying = $state(false);
  let localUsername = $state("");
  let localPassword = $state("");
  let localSubmitting = $state(false);
  let localMode = $state<"sign-in" | "create-account">("sign-in");
  let registrationName = $state("");
  let registrationEmail = $state("");
  let registrationPassword = $state("");
  let registrationSubmitting = $state(false);
  let canCreateLocalAccount = $derived(isLocalRegistrationAvailable(flow.state));
  let canCreateFederatedAccount = $derived(
    isFederatedRegistrationAvailable(flow.state),
  );

  type UserDisplay = {
    origin: string;
    id: string;
    name?: string;
    email?: string;
    image?: string;
  };

  type TechnicalDetail = { label: string; value: string };

  function userDisplayName(user: UserDisplay): string {
    return user.name ?? user.email ?? user.id;
  }

  function userSecondaryIdentity(user: UserDisplay): string | null {
    if (user.email && user.email !== userDisplayName(user)) return user.email;
    return null;
  }

  function avatarInitial(user: UserDisplay): string {
    return userDisplayName(user).trim().charAt(0).toUpperCase() || "?";
  }

  function providerInitial(displayName: string): string {
    return displayName.trim().charAt(0).toUpperCase() || "?";
  }

  function isLocalProvider(providerId: string): boolean {
    return providerId === "local";
  }

  function userImage(user: UserDisplay): string | null {
    return typeof user.image === "string" && user.image.length > 0
      ? user.image
      : null;
  }

  function rawUserId(user: UserDisplay): string {
    return `${user.origin}:${user.id}`;
  }

  function pageUrl(): URL {
    return new URL(window.location.href);
  }

  function redirectLocation(): string | null {
    return portalRedirectLocation(flow.state);
  }

  function showDetachedCompletion(): boolean {
    if (!browser) return false;
    return shouldStayOnPortalCompletionPage(pageUrl(), redirectLocation());
  }

  function returnToApp(): string {
    return portalReturnLocation(flow.state, trellisUrl);
  }

  function shouldShowReturnToAppLink(): boolean {
    if (!browser) return false;
    return shouldOfferPortalReturnLink(pageUrl(), returnToApp());
  }

  function followRedirect(): void {
    const nextLocation = redirectLocation();
    if (nextLocation) {
      if (shouldStayOnPortalCompletionPage(pageUrl(), nextLocation)) {
        return;
      }
      window.location.assign(nextLocation);
    }
  }

  function visibleFlowError(): string | null {
    return visiblePortalFlowError(flow.error, flow.errorCode);
  }

  function setFlowError(message: string): void {
    flow.error = message;
    flow.errorCode = null;
  }

  function clearFlowError(): void {
    flow.error = null;
    flow.errorCode = null;
  }

  async function loadFlow(): Promise<void> {
    const state = await flow.load();
    if (
      !state &&
      shouldShowPortalExpiredState(flow.error, flow.errorCode)
    ) {
      clearFlowError();
      flow.state = { status: "expired" };
      return;
    }
    followRedirect();
  }

  async function approve(): Promise<void> {
    await flow.approve();
    followRedirect();
  }

  async function deny(): Promise<void> {
    if (!flow.flowId) {
      setFlowError("Missing flow id.");
      return;
    }

    denying = true;
    clearFlowError();
    let redirected = false;

    try {
      const nextState = await flow.deny();
      const nextLocation = portalRedirectLocation(nextState);
      if (nextLocation) {
        if (shouldStayOnPortalCompletionPage(pageUrl(), nextLocation)) {
          flow.state = nextState;
          return;
        }
        redirected = true;
        window.location.assign(nextLocation);
        return;
      }
      flow.state = nextState;
    } catch (error) {
      setFlowError(error instanceof Error ? error.message : String(error));
    } finally {
      if (!redirected) denying = false;
    }
  }

  async function submitLocal(): Promise<void> {
    const username = localUsername.trim();
    if (!flow.flowId) {
      setFlowError(
        "This sign-in request has expired. Return to the app and start sign-in again.",
      );
      return;
    }
    if (!username || !localPassword) {
      setFlowError("Enter your username and password.");
      return;
    }

    localSubmitting = true;
    clearFlowError();

    try {
      await submitLocalLogin(trellisUrl, {
        flowId: flow.flowId,
        username,
        password: localPassword,
        portalBindingDigest: flow.binding.digest,
      });
      localPassword = "";
      await loadFlow();
    } catch (error) {
      setFlowError(error instanceof Error ? error.message : String(error));
    } finally {
      localSubmitting = false;
    }
  }

  async function submitLocalCreateAccount(): Promise<void> {
    const username = localUsername.trim();
    const name = registrationName.trim();
    const email = registrationEmail.trim();
    if (!flow.flowId) {
      setFlowError(
        "This sign-in request has expired. Return to the app and start sign-in again.",
      );
      return;
    }
    if (!username || !registrationPassword || !name || !email) {
      setFlowError("Enter your username, password, name, and email.");
      return;
    }

    registrationSubmitting = true;
    clearFlowError();

    try {
      await submitLocalRegistration(trellisUrl, flow.flowId, {
        username,
        password: registrationPassword,
        name,
        email,
        portalBindingDigest: flow.binding.digest,
      });
      registrationPassword = "";
      await loadFlow();
    } catch (error) {
      setFlowError(error instanceof Error ? error.message : String(error));
    } finally {
      registrationSubmitting = false;
    }
  }

  onMount(() => {
    if (!browser) return;
    void loadFlow();
  });
</script>

<svelte:head>
  <title>Sign in · Trellis</title>
</svelte:head>

{#snippet userIdentity(user: UserDisplay)}
  <section
    class="portal-subtle-panel flex items-center gap-3.5 rounded-box p-3.5 sm:gap-4"
  >
    <p class="portal-section-label shrink-0">Signed in as</p>
    <div class="flex min-w-0 flex-1 items-center gap-3.5">
      {#if userImage(user)}
        <img
          class="size-11 rounded-full object-cover"
          src={userImage(user) ?? ""}
          alt=""
        />
      {:else}
        <div
          class="flex size-11 items-center justify-center rounded-full border border-base-300 bg-base-100 text-sm font-semibold text-base-content/70"
          aria-hidden="true"
        >
          {avatarInitial(user)}
        </div>
      {/if}
      <div class="flex min-w-0 items-baseline gap-2">
        <p class="truncate text-[0.98rem] font-semibold leading-5 text-base-content">
          {userDisplayName(user)}
        </p>
        {#if userSecondaryIdentity(user)}
          <span class="shrink-0 text-base-content/25" aria-hidden="true">/</span>
          <p class="portal-copy truncate text-sm leading-5">
            {userSecondaryIdentity(user)}
          </p>
        {/if}
      </div>
    </div>
  </section>
{/snippet}

{#snippet technicalDetails(
  userId: string | null,
  items: TechnicalDetail[],
  capabilityKeys: string[],
)}
  <details class="portal-details portal-debug-details">
    <summary
      class="inline-flex cursor-pointer items-center gap-2 rounded-field text-[0.68rem] leading-5 text-base-content/65"
    >
      {#if userId}
        <span class="mono break-all">{userId}</span>
        <span aria-hidden="true">/</span>
      {/if}
      <span>Technical details</span>
    </summary>
    <dl class="mt-3 grid max-w-xl gap-3 rounded-box border border-base-300 bg-base-100 p-4 text-left text-xs text-base-content/60">
      {#each items as item (item.label)}
        <div>
          <dt class="font-medium text-base-content/65">{item.label}</dt>
          <dd class="mono mt-1 break-all leading-5">{item.value}</dd>
        </div>
      {/each}
      {#if capabilityKeys.length > 0}
        <div>
          <dt class="font-medium text-base-content/65">Raw capability keys</dt>
          <dd>
            <ul class="mt-1 grid gap-1">
              {#each capabilityKeys as key (key)}
                <li class="mono break-all leading-5">{key}</li>
              {/each}
            </ul>
          </dd>
        </div>
      {/if}
    </dl>
  </details>
{/snippet}

<main
  class="portal-shell flex min-h-screen justify-center px-4 py-10 sm:px-6"
  data-theme="portal"
>
  <div
    class={[
      "portal-page w-full",
      flow.state?.status === "approval_required" ||
      flow.state?.status === "insufficient_capabilities"
        ? "max-w-xl"
        : "max-w-md",
    ]}
  >
    <header class="portal-header">
      <PortalBrand subtitle="Login portal" />
    </header>
    <div class="portal-body">
      {#if flow.loading}
        <div class="flex items-center gap-4 py-3">
          <span class="loading loading-ring loading-lg"></span>
          <div>
            <p class="text-sm font-medium text-base-content">Loading request</p>
            <p class="portal-copy text-xs">Resolving provider and approval state.</p>
          </div>
        </div>
      {:else if flow.state?.status === "choose_provider"}
        <div>
          <h1 class="text-center text-xl font-semibold tracking-[-0.025em] text-base-content">Choose a sign-in method</h1>
        </div>
        {#if canCreateFederatedAccount}
          <div class="portal-subtle-panel rounded-box p-3.5 text-sm leading-6 text-base-content/65">
            Continuing with an identity provider can create an account when no
            existing identity is linked.
          </div>
        {/if}
        <div class="flex flex-col gap-2.5">
          {#each flow.state.providers as provider (provider.id)}
            {#if isLocalProvider(provider.id)}
              <div class="portal-subtle-panel grid gap-3 rounded-box p-4">
                <div class="flex items-center justify-between gap-3">
                  <p class="text-sm font-semibold text-base-content">
                    {provider.displayName}
                  </p>
                  {#if canCreateLocalAccount}
                    <div class="join">
                      <button
                        class={["btn join-item btn-xs", localMode === "sign-in" && "btn-active"]}
                        type="button"
                        onclick={() => (localMode = "sign-in")}
                      >
                        Sign in
                      </button>
                      <button
                        class={["btn join-item btn-xs", localMode === "create-account" && "btn-active"]}
                        type="button"
                        onclick={() => (localMode = "create-account")}
                      >
                        Create account
                      </button>
                    </div>
                  {/if}
                </div>

                {#if localMode === "create-account" && canCreateLocalAccount}
                  <form
                    class="grid gap-3"
                    onsubmit={(event) => {
                      event.preventDefault();
                      void submitLocalCreateAccount();
                    }}
                  >
                    <p class="portal-copy text-sm leading-6">
                      Create a local account to continue to {flow.state.app.displayName}.
                    </p>
                    <label class="form-control w-full">
                      <span class="label py-1">
                        <span class="label-text">Name</span>
                      </span>
                      <input
                        class="input input-bordered w-full"
                        autocomplete="name"
                        disabled={registrationSubmitting}
                        required
                        type="text"
                        bind:value={registrationName}
                      />
                    </label>
                    <label class="form-control w-full">
                      <span class="label py-1">
                        <span class="label-text">Email</span>
                      </span>
                      <input
                        class="input input-bordered w-full"
                        autocomplete="email"
                        disabled={registrationSubmitting}
                        required
                        type="email"
                        bind:value={registrationEmail}
                      />
                    </label>
                    <label class="form-control w-full">
                      <span class="label py-1">
                        <span class="label-text">Username</span>
                      </span>
                      <input
                        class="input input-bordered w-full"
                        autocomplete="username"
                        disabled={registrationSubmitting}
                        required
                        type="text"
                        bind:value={localUsername}
                      />
                    </label>
                    <label class="form-control w-full">
                      <span class="label py-1">
                        <span class="label-text">Password</span>
                      </span>
                      <input
                        class="input input-bordered w-full"
                        autocomplete="new-password"
                        disabled={registrationSubmitting}
                        required
                        type="password"
                        bind:value={registrationPassword}
                      />
                    </label>
                    <button
                      class="btn btn-primary btn-block"
                      disabled={registrationSubmitting}
                      type="submit"
                    >
                      {#if registrationSubmitting}
                        <span class="loading loading-spinner loading-sm"></span>
                        Creating account...
                      {:else}
                        Create account
                      {/if}
                    </button>
                  </form>
                {:else}
                  <form
                    class="grid gap-3"
                    onsubmit={(event) => {
                      event.preventDefault();
                      void submitLocal();
                    }}
                  >
                    <label class="form-control w-full">
                      <span class="label py-1">
                        <span class="label-text">Username</span>
                      </span>
                      <input
                        class="input input-bordered w-full"
                        autocomplete="username"
                        disabled={localSubmitting}
                        required
                        type="text"
                        bind:value={localUsername}
                      />
                    </label>
                    <label class="form-control w-full">
                      <span class="label py-1">
                        <span class="label-text">Password</span>
                      </span>
                      <input
                        class="input input-bordered w-full"
                        autocomplete="current-password"
                        disabled={localSubmitting}
                        required
                        type="password"
                        bind:value={localPassword}
                      />
                    </label>
                    <button
                      class="btn btn-primary btn-block"
                      disabled={localSubmitting}
                      type="submit"
                    >
                      {#if localSubmitting}
                        <span class="loading loading-spinner loading-sm"></span>
                        Signing in...
                      {:else}
                        Sign in
                      {/if}
                    </button>
                  </form>
                {/if}
              </div>
            {:else}
              <a
                class="portal-provider-link group flex w-full items-center gap-3 rounded-field px-4 py-3 text-left"
                data-sveltekit-reload
                href={flow.providerUrl(provider.id)}
              >
                <span
                  class="flex size-9 shrink-0 items-center justify-center rounded-full border border-base-300 bg-base-200 text-sm font-semibold text-base-content/70"
                  aria-hidden="true"
                >
                  {providerInitial(provider.displayName)}
                </span>
                <span class="min-w-0 flex-1 truncate text-sm font-semibold text-base-content">
                  {#if isFederatedRegistrationProvider(flow.state, provider.id)}
                    Continue or create account with {provider.displayName}
                  {:else}
                    Continue with {provider.displayName}
                  {/if}
                </span>
                <svg
                  class="size-4 shrink-0 text-base-content/35 transition-transform group-hover:translate-x-0.5"
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  aria-hidden="true"
                >
                  <path
                    fill-rule="evenodd"
                    d="M8.22 5.22a.75.75 0 0 1 1.06 0l4.25 4.25a.75.75 0 0 1 0 1.06l-4.25 4.25a.75.75 0 0 1-1.06-1.06L11.94 10 8.22 6.28a.75.75 0 0 1 0-1.06Z"
                    clip-rule="evenodd"
                  />
                </svg>
              </a>
            {/if}
          {/each}
        </div>
      {:else if flow.state?.status === "processing"}
        <div class="flex items-center gap-4 py-3">
          <span class="loading loading-ring loading-lg"></span>
          <div>
            <p class="text-sm font-medium text-base-content">Finishing sign-in</p>
            <p class="portal-copy text-xs">Applying the authenticated portal policy.</p>
          </div>
        </div>
      {:else if flow.state?.status === "approval_required"}
        <div>
          <h1 class="text-2xl font-semibold tracking-[-0.035em] text-base-content">Approve access</h1>
          <p class="portal-copy mt-2 max-w-[58ch] text-sm">
            Review what <strong class="font-medium text-base-content"
              >{flow.state.approval.displayName}</strong
            > is asking to access from your account.
          </p>
        </div>

        {@render userIdentity(flow.state.user)}

        <section class="space-y-2.5 border-t border-base-300 pt-4">
          <p class="portal-section-label">Requested capabilities</p>
          <ul class="overflow-hidden rounded-box border border-base-300">
            {#each capabilityEntries(flow.state.approval.capabilities) as entry (entry.key)}
              <li class="portal-capability-row px-3.5 py-3">
                <p class="text-sm font-semibold leading-5 text-base-content">
                  {entry.capability.displayName}
                </p>
                <p class="portal-copy mt-0.5 text-sm leading-5">
                  {entry.capability.description}
                </p>
                {#if entry.capability.consequence}
                  <p class="mt-1 text-xs leading-5 text-base-content/65">
                    {entry.capability.consequence}
                  </p>
                {/if}
              </li>
            {:else}
              <li class="portal-subtle-panel rounded-box p-4 text-sm leading-6 text-base-content/65">
                No explicit capabilities are requested beyond identity and
                session binding for this sign-in.
              </li>
            {/each}
          </ul>
        </section>

        <div class="portal-action-row flex flex-col gap-2.5 pt-5">
          <button
            class="btn btn-primary btn-block"
            disabled={flow.loading || denying}
            onclick={() => void approve()}
          >
            {#if flow.loading}
              <span class="loading loading-spinner loading-sm"></span>
            {:else}
              Approve
            {/if}
          </button>
          <button
            class="btn btn-ghost btn-block text-base-content/70"
            disabled={flow.loading || denying}
            onclick={() => void deny()}
          >
            {denying ? "Denying..." : "Deny"}
          </button>
        </div>
      {:else if flow.state?.status === "insufficient_capabilities"}
        <div>
          <h1 class="text-2xl font-semibold tracking-[-0.035em] text-base-content">Access denied</h1>
          <p class="portal-copy mt-2 max-w-[58ch] text-sm">
            An administrator needs to grant the required capabilities before you
            can continue.
            {#if !shouldShowReturnToAppLink()}
              Return to the CLI to finish sign-in or close this page.
            {/if}
          </p>
        </div>
        {#if flow.state.user}
          {@render userIdentity(flow.state.user)}
        {/if}
        <section class="space-y-2.5 border-t border-base-300 pt-4">
          <p class="portal-section-label">Missing capabilities</p>
          <ul class="overflow-hidden rounded-box border border-base-300">
            {#each flow.state.missingCapabilities as cap (cap)}
              {@const metadata = capabilityMetadata(flow.state.approval.capabilities, cap)}
              <li class="portal-capability-row px-3.5 py-3">
                <p class="text-sm font-semibold text-base-content">
                  {portalCapabilityDisplayName(flow.state.approval.capabilities, cap)}
                </p>
                {#if metadata}
                  <p class="portal-copy mt-0.5 text-sm leading-5">
                    {metadata.description}
                  </p>
                  {#if metadata.consequence}
                    <p class="mt-1 text-xs leading-5 text-base-content/65">
                      {metadata.consequence}
                    </p>
                  {/if}
                {/if}
              </li>
            {/each}
          </ul>
        </section>
        {#if shouldShowReturnToAppLink()}
          <a
            class="btn btn-outline btn-block"
            href={returnToApp()}
            >Return to app</a
          >
        {/if}
      {:else if flow.state?.status === "approval_denied"}
        <div>
          <h1 class="text-xl font-semibold tracking-[-0.025em] text-base-content">Access denied</h1>
          <p class="portal-copy mt-2 text-sm">
            You denied access for {flow.state.approval.displayName}. Start a new
            sign-in if you want to review the request again.
            {#if !shouldShowReturnToAppLink()}
              Return to the CLI to finish sign-in or close this page.
            {/if}
          </p>
        </div>
        {#if shouldShowReturnToAppLink()}
          <a
            class="btn btn-outline btn-block"
            href={returnToApp()}
            >Return to app</a
          >
        {/if}
      {:else if flow.state?.status === "expired"}
        <div>
          <h1 class="text-xl font-semibold tracking-[-0.025em] text-base-content">Session expired</h1>
          <p class="portal-copy mt-2 text-sm">
            This sign-in request is no longer active.
            {#if shouldShowReturnToAppLink()}
              Return to the app and start sign-in again.
            {:else}
              Return to the CLI or app and start sign-in again.
            {/if}
          </p>
        </div>
        <a
          class="btn btn-outline btn-block"
          href={returnToApp()}
          >{shouldShowReturnToAppLink() ? "Return to app" : "Open Trellis"}</a
        >
      {:else if flow.state?.status === "redirect"}
        {#if showDetachedCompletion()}
          <div>
            <h1 class="text-xl font-semibold tracking-[-0.025em] text-base-content">Connected</h1>
            <p class="portal-copy mt-2 text-sm">
              Return to the CLI to finish sign-in.
            </p>
          </div>
        {/if}
      {/if}

      {#if visibleFlowError()}
        <div class="alert alert-error text-sm">
          <span>{visibleFlowError()}</span>
        </div>
      {/if}
    </div>
  </div>
  {#if flow.state?.status === "approval_required"}
    {@render technicalDetails(
      rawUserId(flow.state.user),
      [
        { label: "Request handle", value: flow.state.flowId },
        { label: "Contract id", value: flow.state.approval.contractId },
        { label: "Contract digest", value: flow.state.approval.contractDigest },
        { label: "Signed-in user id", value: rawUserId(flow.state.user) },
      ],
      capabilityEntries(flow.state.approval.capabilities).map((entry) => entry.key),
    )}
  {:else if flow.state?.status === "insufficient_capabilities" && flow.state.user}
    {@render technicalDetails(
      rawUserId(flow.state.user),
      [
        { label: "Request handle", value: flow.state.flowId },
        { label: "Contract id", value: flow.state.approval.contractId },
        { label: "Contract digest", value: flow.state.approval.contractDigest },
        { label: "Signed-in user id", value: rawUserId(flow.state.user) },
      ],
      flow.state.missingCapabilities,
    )}
  {/if}
</main>
