<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onDestroy, untrack } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { formatDate, humanizeName } from "$lib/format";
  import CopyButton from "./CopyButton.svelte";
  import LoadingState from "./LoadingState.svelte";
  import Notice from "./Notice.svelte";
  import Panel from "./Panel.svelte";

  let {
    userId,
    userLabel,
    disabled = false,
  }: {
    userId: apis.auth.UsersGetInput["userId"];
    userLabel: string;
    disabled?: boolean;
  } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-security");
  let sessions = $state.raw<apis.auth.AuthorizationSessionsListOutput["items"]>(
    [],
  );
  let loading = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let reset = $state.raw<apis.auth.UsersPasswordResetCreateOutput | null>(null);

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const principalId = userId;
      const result = await traverseAll(async (page) => {
        const response = await trellis
          .authorizationSessionsList({
            principalId,
            page: catalogPage(page.cursor),
          })
          .orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      sessions = [...result.items];
      if (!result.complete) error = projectConsoleError(result.error).message;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function revoke(session: (typeof sessions)[number]): Promise<void> {
    if (
      busy ||
      disabled ||
      !canPerform(authority.authority, "authorizationSessionsRevoke")
    )
      return;
    if (
      !window.confirm(
        `Revoke authorization session ${session.authorizationSessionId} for ${userLabel}? Its runtime attachments lose authority.`,
      )
    )
      return;
    const key = scope.key;
    busy = true;
    try {
      await trellis
        .authorizationSessionsRevoke({
          authorizationSessionId: session.authorizationSessionId,
          idempotencyKey: ulid(),
        })
        .orThrow();
      if (scope.key === key) await load();
    } catch (cause) {
      if (scope.key === key) error = projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  async function createReset(): Promise<void> {
    if (
      busy ||
      disabled ||
      !canPerform(authority.authority, "usersPasswordResetCreate")
    )
      return;
    const key = scope.key;
    busy = true;
    reset = null;
    try {
      const result = await trellis
        .usersPasswordResetCreate({ userId, idempotencyKey: ulid() })
        .orThrow();
      if (scope.key === key) reset = result;
    } catch (cause) {
      if (scope.key === key) error = projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    scope.setKey(userId);
    sessions = [];
    reset = null;
    untrack(() => void load());
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<Panel title="Sign-ins">
  <p class="mb-3 text-sm text-base-content/65">
    Where this person is signed in.
  </p>
  {#if error}<Notice variant="error"
      >{error}<button
        class="btn btn-ghost btn-xs ml-2"
        onclick={() => void load()}>Retry</button
      ></Notice
    >{/if}
  {#if loading}<LoadingState label="Loading sign-ins" />{:else}
    <ul class="divide-y divide-base-300 rounded-field border border-base-300">
      {#each sessions.filter((session) => session.state === "active") as session (session.authorizationSessionId)}
        <li class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
          <span>
            <span class="font-medium"
              >{humanizeName(
                session.binding.clientId ??
                  session.binding.participantId ??
                  "App",
              )}</span
            >
            <span class="text-base-content/60">
              · signed in {formatDate(session.createdAt)}</span
            >
          </span>
          <button
            class="btn btn-ghost btn-xs text-error"
            disabled={disabled ||
              busy ||
              !canPerform(authority.authority, "authorizationSessionsRevoke")}
            onclick={() => void revoke(session)}>Sign out</button
          >
        </li>
      {:else}
        <li class="px-3 py-2 text-sm text-base-content/60">
          Not signed in anywhere.
        </li>
      {/each}
    </ul>
  {/if}
  <div class="mt-4">
    <button
      class="btn btn-outline btn-sm"
      disabled={disabled ||
        busy ||
        !canPerform(authority.authority, "usersPasswordResetCreate")}
      onclick={() => void createReset()}>Reset password</button
    >
    {#if reset}<p class="trellis-field-help mt-2">
        Send this one-time code to the person. It expires {formatDate(
          reset.expiresAt,
        )}.
      </p>
      <p class="trellis-identifier break-all">
        {reset.token}<CopyButton
          value={reset.token}
          label="Copy password-reset token"
        />
      </p>{/if}
  </div>
</Panel>
