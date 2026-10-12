<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { ulid } from "ulid";
  import { SvelteSet } from "svelte/reactivity";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { runBulk, bulkExpectedCount } from "$lib/bulk.ts";
  import { formatDate } from "$lib/format";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";

  type Session = apis.auth.AuthorizationSessionsListOutput["items"][number];
  type Attachment = apis.auth.ConnectionsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const sessionScope = new RequestScope("authorization-sessions");
  const attachmentScope = new RequestScope("attachments");
  let sessions = $state.raw<Session[]>([]);
  let attachments = $state.raw<Attachment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let attachmentError = $state<string | null>(null);
  let tab = $state<"sessions" | "connections">("sessions");
  const selected = new SvelteSet<string>();
  let busy = $state(false);
  let uncertain = $state(false);
  let result = $state<{ succeeded: number; failed: string[] } | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();

  async function load(): Promise<void> {
    const st = sessionScope.begin();
    const at = attachmentScope.begin();
    loading = true;
    const [sessionResult, attachmentResult] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis
          .authorizationSessionsList({ page })
          .orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.connectionsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    if (sessionScope.settle(st)) {
      sessions = [...sessionResult.items];
      error = sessionResult.complete
        ? null
        : projectConsoleError(sessionResult.error).message;
      loading = false;
    }
    if (attachmentScope.settle(at)) {
      attachments = [...attachmentResult.items];
      attachmentError = attachmentResult.complete
        ? null
        : projectConsoleError(attachmentResult.error).message;
    }
  }

  async function bulkAction(): Promise<void> {
    const operation = tab;
    if (
      busy ||
      uncertain ||
      !canPerform(
        authority.authority,
        operation === "sessions"
          ? "authorizationSessionsRevoke"
          : "connectionsKick",
      )
    )
      return;
    const targets =
      operation === "sessions"
        ? sessions
            .filter(
              (item) =>
                selected.has(item.authorizationSessionId) &&
                item.state === "active",
            )
            .map((item) => ({ id: item.authorizationSessionId, key: ulid() }))
        : attachments
            .filter(
              (item) =>
                selected.has(item.attachmentId) && item.state === "admitted",
            )
            .map((item) => ({ id: item.attachmentId, key: ulid() }));
    if (!targets.length) return;
    const confirmed = await confirmationModal?.confirm({
      title:
        operation === "sessions"
          ? "Revoke selected sessions?"
          : "Kick selected attachments?",
      message:
        operation === "sessions"
          ? "Retirement removes logical authority and queues physical connection enforcement."
          : "A kick closes the physical attachment; it does not revoke the grant or logical session.",
      confirmLabel: operation === "sessions" ? "Revoke" : "Kick",
      targetLabel: "Targets",
      targetName: `${targets.length} targets`,
      expectedValue: bulkExpectedCount(targets.length),
    });
    if (!confirmed || tab !== operation || sessionScope.disposed) return;
    busy = true;
    const outcome = await runBulk(
      targets,
      async (target) => {
        if (sessionScope.disposed || tab !== operation)
          throw new Error("Page target changed before dispatch.");
        if (operation === "sessions")
          await trellis
            .authorizationSessionsRevoke({
              authorizationSessionId: target.id,
              idempotencyKey: target.key,
            })
            .orThrow();
        else
          await trellis
            .connectionsKick({
              attachmentId: target.id,
              idempotencyKey: target.key,
            })
            .orThrow();
      },
      (cause) =>
        classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed",
    );
    if (sessionScope.disposed) return;
    result = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((item) => `${item.target.id}: ${item.reason}`),
    };
    uncertain = outcome.unknown.length > 0;
    busy = false;
    selected.clear();
    await load();
  }
  onMount(() => {
    void load();
    return () => {
      sessionScope.dispose();
      attachmentScope.dispose();
    };
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="Sessions"
    description="Logical authorization sessions and physical runtime attachments are separate lifecycles."
    >{#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        disabled={loading}
        onclick={() => void load()}>Refresh</button
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant="error">Session catalog incomplete: {error}</Notice
    >{/if}
  {#if attachmentError}<Notice variant="warning"
      >Attachment catalog incomplete: {attachmentError}</Notice
    >{/if}
  {#if uncertain}<Notice variant="warning"
      >Bulk outcome unknown. Inspect the targets before issuing another action.</Notice
    >{/if}
  <div class="flex gap-2">
    <button
      class="btn btn-sm {tab === 'sessions' ? 'btn-active' : 'btn-ghost'}"
      onclick={() => {
        tab = "sessions";
        selected.clear();
      }}>Authorization sessions</button
    ><button
      class="btn btn-sm {tab === 'connections' ? 'btn-active' : 'btn-ghost'}"
      onclick={() => {
        tab = "connections";
        selected.clear();
      }}>Connections</button
    >
  </div>
  {#if result}<BulkResult
      succeeded={result.succeeded}
      failed={result.failed}
      pastTense={tab === "sessions" ? "sessions revoked" : "attachments kicked"}
      onDismiss={() => {
        result = null;
      }}
    />{/if}
  {#if selected.size}<BulkActionBar
      count={selected.size}
      noun={tab === "sessions" ? "session" : "attachment"}
      onClear={() => selected.clear()}
      >{#snippet actions()}<button
          class="btn btn-error btn-outline btn-sm"
          disabled={busy || uncertain}
          onclick={() => void bulkAction()}
          >{tab === "sessions" ? "Revoke selected" : "Kick selected"}</button
        >{/snippet}</BulkActionBar
    >{/if}
  <Panel
    title={tab === "sessions"
      ? "Authorization sessions"
      : "Physical attachments"}
    eyebrow={`${tab === "sessions" ? sessions.length : attachments.length} loaded records`}
  >
    {#if loading}<LoadingState
        label="Loading sessions"
      />{:else if tab === "sessions"}
      <DataTable
        ><thead
          ><tr
            ><th>Select</th><th>Session / Principal</th><th
              >Binding / Runtime</th
            ><th>State</th><th>Expiry</th><th>Actions</th></tr
          ></thead
        ><tbody>
          {#each sessions as session (session.authorizationSessionId)}<tr
              ><td
                ><input
                  class="checkbox checkbox-xs"
                  type="checkbox"
                  checked={selected.has(session.authorizationSessionId)}
                  aria-label={`Select ${session.authorizationSessionId}`}
                  disabled={busy || session.state !== "active"}
                  onchange={(event) => {
                    if (event.currentTarget.checked)
                      selected.add(session.authorizationSessionId);
                    else selected.delete(session.authorizationSessionId);
                  }}
                /></td
              ><td class="trellis-identifier"
                >{session.authorizationSessionId}
                <div class="trellis-metadata">
                  {session.principalId} · {session.principalKind}
                </div></td
              ><td
                >{session.binding.kind}
                <div class="trellis-identifier">
                  {session.binding.clientId ??
                    session.binding.deploymentId ??
                    ""}
                </div>
                <div class="trellis-metadata">{session.runtimeId}</div></td
              ><td
                ><span
                  class="badge badge-sm {session.state === 'active'
                    ? 'badge-success'
                    : 'badge-neutral'}">{session.state}</span
                >{#if session.reason}<div class="trellis-metadata">
                    {session.reason}
                  </div>{/if}</td
              ><td>{formatDate(session.notAfter)}</td><td
                >{#if session.state === "active"}<a
                    class="btn btn-ghost btn-xs"
                    href={resolve(
                      `/admin/sessions/revoke?authorizationSessionId=${session.authorizationSessionId}`,
                    )}>Revoke</a
                  >{/if}</td
              ></tr
            >{:else}<tr
              ><td colspan="6">No authorization sessions returned.</td></tr
            >{/each}
        </tbody></DataTable
      >
    {:else}
      <DataTable
        ><thead
          ><tr
            ><th>Select</th><th>Attachment / Session</th><th
              >Broker server / Client</th
            ><th>State</th><th>Admitted</th><th>Actions</th></tr
          ></thead
        ><tbody>
          {#each attachments as attachment (attachment.attachmentId)}<tr
              ><td
                ><input
                  class="checkbox checkbox-xs"
                  type="checkbox"
                  checked={selected.has(attachment.attachmentId)}
                  aria-label={`Select ${attachment.attachmentId}`}
                  disabled={busy || attachment.state !== "admitted"}
                  onchange={(event) => {
                    if (event.currentTarget.checked)
                      selected.add(attachment.attachmentId);
                    else selected.delete(attachment.attachmentId);
                  }}
                /></td
              ><td class="trellis-identifier"
                >{attachment.attachmentId}
                <div class="trellis-metadata">
                  {attachment.authorizationSessionId}
                </div></td
              ><td class="trellis-identifier"
                >{attachment.serverId}
                <div>{attachment.brokerClientId}</div></td
              ><td
                ><span
                  class="badge badge-sm {attachment.state === 'admitted'
                    ? 'badge-success'
                    : 'badge-neutral'}">{attachment.state}</span
                ></td
              ><td
                >{attachment.admittedAt
                  ? formatDate(attachment.admittedAt)
                  : "Not admitted"}</td
              ><td
                >{#if attachment.state === "admitted"}<a
                    class="btn btn-ghost btn-xs"
                    href={resolve(
                      `/admin/sessions/kick?attachmentId=${attachment.attachmentId}`,
                    )}>Kick</a
                  >{/if}</td
              ></tr
            >{:else}<tr><td colspan="6">No attachments returned.</td></tr
            >{/each}
        </tbody></DataTable
      >
    {/if}
  </Panel>
</section>
<ConfirmationModal bind:this={confirmationModal} />
