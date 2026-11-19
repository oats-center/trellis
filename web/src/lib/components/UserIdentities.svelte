<script lang="ts">
  import { apis } from "$lib/trellis-apis";
  import { ulid } from "ulid";
  import { onDestroy, untrack } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { formatDate } from "$lib/format";
  import ConfirmationModal from "./ConfirmationModal.svelte";
  import LoadingState from "./LoadingState.svelte";
  import Notice from "./Notice.svelte";
  import Panel from "./Panel.svelte";

  type User = apis.auth.UsersGetOutput["user"];
  type Identity = apis.auth.UsersIdentitiesLinkOutput["identities"][number];

  let {
    user,
    disabled = false,
    onchange,
  }: {
    /** The account, with the revision a link or unlink is checked against. */
    user: User;
    disabled?: boolean;
    /** The account after a link, an unlink or a reload, with its current revision. */
    onchange: (user: User) => void;
  } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-identities");
  const userId = $derived(user.principalId);
  let identities = $state.raw<Identity[]>([]);
  let providers = $state.raw<
    apis.auth.UsersIdentityLinkProvidersOutput["providers"]
  >([]);
  let loading = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let providerId = $state("");
  let subject = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();
  const canLink = $derived(
    canPerform(authority.authority, "usersIdentitiesLink"),
  );
  const canUnlink = $derived(
    canPerform(authority.authority, "usersIdentitiesUnlink"),
  );

  function providerTitle(identity: Identity): string {
    return (
      providers.find((provider) => provider.providerId === identity.providerId)
        ?.title ?? identity.issuer
    );
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const [found, offered] = await Promise.all([
        trellis.usersFind({ userId }).orThrow(),
        trellis.usersIdentityLinkProviders({}).orThrow(),
      ]);
      if (!scope.isCurrent(token)) return;
      identities = found.found?.identities ?? [];
      // Link and unlink check the account's revision, which another
      // administrator may have moved on since the page loaded.
      if (found.found) onchange(found.found.user);
      providers = offered.providers;
      if (!providers.some((provider) => provider.providerId === providerId))
        providerId = providers[0]?.providerId ?? "";
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function change(
    send: (
      revision: NonNullable<User["account"]>["principal"]["revision"],
    ) => Promise<apis.auth.UsersIdentitiesLinkOutput>,
  ): Promise<boolean> {
    const revision = user.account?.principal.revision;
    if (busy || disabled || revision === undefined) return false;
    const key = scope.key;
    busy = true;
    error = null;
    try {
      const result = await send(revision);
      if (scope.key !== key) return false;
      identities = result.identities;
      onchange(result.user);
      return true;
    } catch (cause) {
      if (scope.key === key) error = projectConsoleError(cause).message;
      return false;
    } finally {
      busy = false;
    }
  }

  async function link(): Promise<void> {
    const linked = subject.trim();
    const provider = providers.find((item) => item.providerId === providerId);
    if (!canLink || !provider || !linked) return;
    const done = await change(
      async (expectedRevision) =>
        await trellis
          .usersIdentitiesLink({
            userId: user.principalId,
            identity: { providerId: provider.providerId, subject: linked },
            expectedRevision,
            idempotencyKey: ulid(),
          })
          .orThrow(),
    );
    if (done) subject = "";
  }

  async function unlink(identity: Identity): Promise<void> {
    if (!canUnlink) return;
    const title = providerTitle(identity);
    const confirmed = await confirmationModal?.confirm({
      title: `Unlink this ${title} sign-in?`,
      message: `${user.displayName} will no longer be able to sign in with the ${title} account ${identity.subject}, and the sign-ins made with it end.`,
      confirmLabel: "Unlink",
    });
    if (!confirmed) return;
    await change(
      async (expectedRevision) =>
        await trellis
          .usersIdentitiesUnlink({
            userId: user.principalId,
            identity: {
              providerId: identity.providerId,
              subject: identity.subject,
            },
            expectedRevision,
            idempotencyKey: ulid(),
          })
          .orThrow(),
    );
  }

  $effect(() => {
    scope.setKey(userId);
    identities = [];
    untrack(() => void load());
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<Panel title="OIDC sign-ins">
  <p class="mb-3 text-sm text-base-content/65">
    The provider accounts this person signs in with.
  </p>
  {#if error}<Notice variant="error" role="alert"
      >{error}<button
        class="btn btn-ghost btn-xs ml-2"
        onclick={() => void load()}>Reload</button
      ></Notice
    >{/if}
  {#if loading}<LoadingState label="Loading OIDC sign-ins" />{:else}
    <ul class="divide-y divide-base-300 rounded-field border border-base-300">
      {#each identities as identity (identity.identityId)}
        <li class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
          <span class="min-w-0">
            <span class="font-medium">{providerTitle(identity)}</span>
            <span class="trellis-identifier break-all">{identity.subject}</span>
            <span class="text-base-content/60">
              · linked {formatDate(identity.linkedAt)}</span
            >
          </span>
          {#if canUnlink}<button
              class="btn btn-ghost btn-xs text-error"
              disabled={disabled || busy}
              onclick={() => void unlink(identity)}>Unlink</button
            >{/if}
        </li>
      {:else}
        <li class="px-3 py-2 text-sm text-base-content/60">
          No OIDC sign-ins linked.
        </li>
      {/each}
    </ul>
    {#if canLink && providers.length > 0}
      <form
        class="mt-4 grid gap-3 md:grid-cols-[minmax(0,12rem)_minmax(0,1fr)_auto] md:items-end"
        onsubmit={(event) => {
          event.preventDefault();
          void link();
        }}
      >
        <label class="form-control"
          ><span class="trellis-field-label">Provider</span><select
            class="select select-bordered select-sm mt-1"
            bind:value={providerId}
            disabled={disabled || busy}
          >
            {#each providers as provider (provider.providerId)}
              <option value={provider.providerId}>{provider.title}</option>
            {/each}
          </select></label
        >
        <label class="form-control"
          ><span class="trellis-field-label">Subject</span><input
            class="input input-bordered input-sm mt-1"
            bind:value={subject}
            required
            autocomplete="off"
            spellcheck="false"
            disabled={disabled || busy}
          /></label
        >
        <button
          class="btn btn-outline btn-sm"
          type="submit"
          disabled={disabled || busy || !subject.trim()}
          >{busy ? "Linking…" : "Link sign-in"}</button
        >
      </form>
      <p class="trellis-field-help mt-2">
        The subject is the provider's <code>sub</code> for this person. Their next
        sign-in with it opens this account.
      </p>
    {/if}
  {/if}
</Panel>
<ConfirmationModal bind:this={confirmationModal} />
