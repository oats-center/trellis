<script lang="ts">
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { apis } from "$lib/trellis-apis";
  import { getTrellis } from "$lib/trellis";
  import { formatDate } from "$lib/format";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ActionMenu from "./ActionMenu.svelte";
  import CopyButton from "./CopyButton.svelte";
  import DataTable from "./DataTable.svelte";
  import Icon from "./Icon.svelte";
  import Notice from "./Notice.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  type Secret = apis.auth.EnrollmentSecretsListOutput["items"][number];
  type Policy = apis.auth.InstancePolicyGetOutput;

  let {
    deploymentId,
    serviceTitle,
    enabled,
    onchange,
  }: {
    deploymentId: string;
    serviceTitle: string;
    /** Whether the service is enabled; a disabled one refuses enrollment. */
    enabled: boolean;
    /** Called after a change that can affect the instance list. */
    onchange: () => void;
  } = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const ALERTS: Record<Secret["alerts"][number]["kind"], string> = {
    revokedUsed:
      "Someone used it after it was revoked, so a copy is still out there.",
    expiredUsed:
      "Someone used it after it expired, so a copy is still out there.",
    instanceLimit: "It reached its instance limit.",
    rateLimited: "It reached its hourly limit.",
    participantMismatch: "A process of another service tried to use it.",
    buildMismatch: "A process with a different build tried to use it.",
  };

  let secrets = $state.raw<Secret[]>([]);
  let policy = $state.raw<Policy | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let issuing = $state(false);
  let editing = $state<Secret | null>(null);
  let revoking = $state<Secret | null>(null);
  let retireInstances = $state(false);
  let shown = $state<string | null>(null);
  let form = $state({
    title: "",
    maxInstances: 20,
    perHour: 100,
    expires: "",
    currentBuildOnly: false,
  });
  let retirement = $state<"default" | "off" | "after">("default");
  let retirementHours = $state(24);
  let provisioned = $state<"default" | "on" | "off">("default");

  const live = $derived(secrets.filter((secret) => !secret.revokedAt));
  const alerted = $derived(secrets.filter((secret) => secret.alerts.length));
  const canIssue = $derived(
    canPerform(authority.authority, "enrollmentSecretsIssue"),
  );

  function isExpired(secret: Secret): boolean {
    return !!secret.expiresAt && Date.parse(secret.expiresAt) <= Date.now();
  }

  async function load(): Promise<void> {
    try {
      const listAll = async (): Promise<Secret[]> => {
        const all: Secret[] = [];
        let cursor: string | undefined;
        do {
          const listed = await trellis
            .enrollmentSecretsList({
              deploymentId: codecs.ulid.decode(deploymentId),
              page: { limit: 200, ...(cursor ? { cursor } : {}) },
            })
            .orThrow();
          all.push(...listed.items);
          cursor = listed.page.nextCursor;
        } while (cursor);
        return all;
      };
      const [listed, read] = await Promise.all([
        listAll(),
        trellis
          .instancePolicyGet({ deploymentId: codecs.ulid.decode(deploymentId) })
          .orThrow(),
      ]);
      secrets = listed;
      policy = read;
      const own = read.deployment;
      retirement =
        own?.retireOffline === false
          ? "off"
          : own?.retireOfflineAfterSeconds !== undefined
            ? "after"
            : "default";
      retirementHours = Math.round(
        (own?.retireOfflineAfterSeconds ??
          read.installation.retireOfflineAfterSeconds) / 3600,
      );
      provisioned =
        own?.retireProvisioned === undefined
          ? "default"
          : own.retireProvisioned
            ? "on"
            : "off";
      error = null;
    } catch (cause) {
      error = projectConsoleError(cause).message;
    }
  }

  $effect(() => {
    void deploymentId;
    void load();
  });

  function startIssue(): void {
    editing = null;
    shown = null;
    form = {
      title: serviceTitle,
      maxInstances: policy?.installation.secretMaxInstances ?? 20,
      perHour: policy?.installation.secretEnrollmentsPerHour ?? 100,
      expires: "",
      currentBuildOnly: false,
    };
    issuing = true;
  }

  function startEdit(secret: Secret): void {
    issuing = false;
    shown = null;
    form = {
      title: secret.title,
      maxInstances: secret.maxInstances,
      perHour: secret.enrollmentsPerHour,
      expires: secret.expiresAt?.slice(0, 10) ?? "",
      currentBuildOnly: secret.currentBuildOnly,
    };
    editing = secret;
  }

  function expiry(): string | undefined {
    return form.expires
      ? new Date(`${form.expires}T00:00:00Z`).toISOString()
      : undefined;
  }

  async function run(change: () => Promise<void>, unknown: string) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await change();
      await load();
      onchange();
    } catch (cause) {
      error =
        classifyMutationError(cause).kind === "unknown"
          ? unknown
          : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  function issue(): Promise<void> {
    return run(async () => {
      const issued = await trellis
        .enrollmentSecretsIssue(
          apis.auth.API.actions["rpc:EnrollmentSecrets.Issue"].input.decode({
            deploymentId,
            title: form.title.trim(),
            maxInstances: form.maxInstances,
            enrollmentsPerHour: form.perHour,
            currentBuildOnly: form.currentBuildOnly,
            ...(expiry() ? { expiresAt: expiry() } : {}),
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        )
        .orThrow();
      issuing = false;
      shown = issued.text ?? null;
    }, "It's unknown whether the secret was issued. Check the list; a secret that was issued can't be shown again.");
  }

  function save(secret: Secret): Promise<void> {
    return run(async () => {
      // The date field shows only the day, so an untouched field keeps the
      // exact expiry instead of moving it to that day's midnight.
      const edited = expiry();
      const expires =
        form.expires === (secret.expiresAt?.slice(0, 10) ?? "")
          ? secret.expiresAt
          : edited && codecs.timestamp.decode(edited);
      await trellis
        .enrollmentSecretsUpdate({
          secretId: secret.secretId,
          title: form.title.trim(),
          maxInstances: form.maxInstances,
          enrollmentsPerHour: form.perHour,
          currentBuildOnly: form.currentBuildOnly,
          ...(expires ? { expiresAt: expires } : {}),
          expectedRevision: secret.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      editing = null;
    }, "It's unknown whether the change was saved. Reload to check.");
  }

  function revoke(secret: Secret): Promise<void> {
    return run(async () => {
      await trellis
        .enrollmentSecretsRevoke({
          secretId: secret.secretId,
          ...(retireInstances ? { retireInstances: true } : {}),
          expectedRevision: secret.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      revoking = null;
      retireInstances = false;
    }, "It's unknown whether the secret was revoked. Reload to check.");
  }

  function saveRetirement(): Promise<void> {
    return run(async () => {
      const own = {
        ...(retirement === "off" ? { retireOffline: false } : {}),
        ...(retirement === "after"
          ? {
              retireOffline: true,
              retireOfflineAfterSeconds: Math.max(1, retirementHours) * 3600,
            }
          : {}),
        ...(provisioned === "default"
          ? {}
          : { retireProvisioned: provisioned === "on" }),
      };
      await trellis
        .instancePolicySet(
          apis.auth.API.actions["rpc:InstancePolicy.Set"].input.decode({
            deploymentId,
            ...(Object.keys(own).length ? { deployment: own } : {}),
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        )
        .orThrow();
    }, "It's unknown whether the change was saved. Reload to check.");
  }
</script>

{#snippet limitsForm(submitLabel: string, submit: () => Promise<void>)}
  <form
    class="space-y-3 px-5 pb-4"
    aria-label={submitLabel}
    onsubmit={(event) => {
      event.preventDefault();
      void submit();
    }}
  >
    <label class="block"
      ><span class="trellis-field-label">Title</span><input
        class="input input-sm w-full"
        bind:value={form.title}
        required
      /></label
    >
    <div class="grid gap-3 sm:grid-cols-3">
      <label class="block"
        ><span class="trellis-field-label">Instances at once</span><input
          class="input input-sm w-full"
          type="number"
          min="1"
          bind:value={form.maxInstances}
        /></label
      >
      <label class="block"
        ><span class="trellis-field-label">New instances an hour</span><input
          class="input input-sm w-full"
          type="number"
          min="1"
          bind:value={form.perHour}
        /></label
      >
      <label class="block"
        ><span class="trellis-field-label">Stops working on</span><input
          class="input input-sm w-full"
          type="date"
          bind:value={form.expires}
        /></label
      >
    </div>
    <label class="flex items-start gap-2 text-sm"
      ><input
        type="checkbox"
        class="checkbox checkbox-sm mt-0.5"
        bind:checked={form.currentBuildOnly}
      /><span
        >Only the installed build<span
          class="block text-xs text-base-content/60"
          >Its instances can't install a new build themselves. Turn this on when
          you install each build before rolling it out.</span
        ></span
      ></label
    >
    <div class="flex gap-2">
      <button
        class="btn btn-primary btn-sm"
        type="submit"
        disabled={busy || !form.title.trim()}>{submitLabel}</button
      >
      <button
        class="btn btn-ghost btn-sm"
        type="button"
        onclick={() => {
          issuing = false;
          editing = null;
        }}>Cancel</button
      >
    </div>
  </form>
{/snippet}

<section class="trellis-section" aria-labelledby="enrollment-title">
  <div class="flex items-center justify-between gap-3 px-5 py-4">
    <h2 id="enrollment-title" class="text-base font-semibold">Enrollment</h2>
    {#if canIssue && enabled}<button
        class="btn btn-outline btn-sm"
        type="button"
        disabled={busy}
        onclick={startIssue}
        ><Icon name="plus" size={16} />Issue a secret</button
      >{/if}
  </div>
  {#if error}<Notice variant="error" class="mx-5 mb-3">{error}</Notice>{/if}
  {#each alerted as secret (secret.secretId)}
    <Notice variant="warning" class="mx-5 mb-3" role="status"
      ><strong>{secret.title}</strong>: {secret.alerts
        .map((alert) => ALERTS[alert.kind])
        .join(" ")}</Notice
    >
  {/each}
  {#if issuing}{@render limitsForm("Issue secret", issue)}{/if}
  {#if shown}
    <div class="mx-5 mb-4 space-y-2 rounded-lg border border-base-300 p-4">
      <p class="text-sm font-medium">
        Copy the secret now. You won't see it again.
      </p>
      <p class="flex items-center gap-1">
        <code class="trellis-identifier break-all" data-enrollment-secret
          >{shown}</code
        ><CopyButton value={shown} label="Copy secret" />
      </p>
      <p class="flex items-center gap-1 text-xs text-base-content/70">
        <code class="break-all">TRELLIS_ENROLLMENT_SECRET={shown}</code
        ><CopyButton
          value={`TRELLIS_ENROLLMENT_SECRET=${shown}`}
          label="Copy environment line"
        />
      </p>
      <button
        class="btn btn-ghost btn-sm"
        type="button"
        onclick={() => (shown = null)}>Done</button
      >
    </div>
  {/if}
  {#if secrets.length}
    <DataTable class="border-t border-base-300">
      <thead>
        <tr>
          <th class="trellis-primary">Secret</th>
          <th><span class="max-sm:sr-only">State</span></th>
          <th class="hidden sm:table-cell">Instances</th>
          <th class="hidden sm:table-cell">Last used</th>
          <th><span class="sr-only">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each secrets as secret (secret.secretId)}
          <tr data-secret-id={secret.secretId}>
            <td class="trellis-primary">
              <span class="block font-medium">{secret.title}</span>
              <span class="block text-xs text-base-content/60"
                >Up to {secret.maxInstances} at once, {secret.enrollmentsPerHour}
                an hour{secret.currentBuildOnly
                  ? ", installed build only"
                  : ""}{secret.expiresAt
                  ? `, until ${formatDate(secret.expiresAt)}`
                  : ""}</span
              >
            </td>
            <td
              ><StatusBadge
                compact
                label={secret.revokedAt
                  ? "Revoked"
                  : isExpired(secret)
                    ? "Expired"
                    : "Active"}
                status={secret.revokedAt || isExpired(secret)
                  ? "offline"
                  : "healthy"}
              /></td
            >
            <td class="hidden sm:table-cell"
              >{secret.instances} of {secret.maxInstances}</td
            >
            <td class="hidden whitespace-nowrap sm:table-cell"
              >{secret.lastUsedAt ? formatDate(secret.lastUsedAt) : "Never"}</td
            >
            <td class="text-right">
              {#if !secret.revokedAt}
                <ActionMenu
                  widthClass="w-40"
                  ariaLabel={`Actions for ${secret.title}`}
                >
                  <li>
                    <button
                      disabled={busy ||
                        !canPerform(
                          authority.authority,
                          "enrollmentSecretsUpdate",
                        )}
                      onclick={() => startEdit(secret)}>Edit</button
                    >
                  </li>
                  <li>
                    <button
                      class="text-error"
                      disabled={busy ||
                        !canPerform(
                          authority.authority,
                          "enrollmentSecretsRevoke",
                        )}
                      onclick={() => {
                        revoking = secret;
                        retireInstances = false;
                      }}>Revoke</button
                    >
                  </li>
                </ActionMenu>
              {/if}
            </td>
          </tr>
          {#if editing?.secretId === secret.secretId}
            <tr
              ><td colspan="5"
                >{@render limitsForm("Save", () => save(secret))}</td
              ></tr
            >
          {/if}
          {#if revoking?.secretId === secret.secretId}
            <tr
              ><td colspan="5">
                <form
                  class="space-y-3"
                  aria-label="Revoke secret"
                  onsubmit={(event) => {
                    event.preventDefault();
                    void revoke(secret);
                  }}
                >
                  <p class="text-sm">
                    Revoke {secret.title}? It creates no more instances.
                  </p>
                  {#if secret.instances > 0}<label
                      class="flex items-center gap-2 text-sm"
                      ><input
                        type="checkbox"
                        class="checkbox checkbox-sm"
                        bind:checked={retireInstances}
                      />Also retire and disconnect its {secret.instances}
                      {secret.instances === 1n
                        ? "instance"
                        : "instances"}</label
                    >{/if}
                  <div class="flex gap-2">
                    <button
                      class="btn btn-error btn-sm"
                      type="submit"
                      disabled={busy}>Revoke secret</button
                    >
                    <button
                      class="btn btn-ghost btn-sm"
                      type="button"
                      onclick={() => (revoking = null)}>Cancel</button
                    >
                  </div>
                </form>
              </td></tr
            >
          {/if}
        {/each}
      </tbody>
    </DataTable>
  {:else if !issuing && !shown}
    <p class="px-5 pb-4 text-sm text-base-content/60">
      {live.length === 0 && canIssue
        ? "Issue a secret to let processes add themselves as instances."
        : "No enrollment secrets."}
    </p>
  {/if}
  {#if policy && canPerform(authority.authority, "instancePolicySet")}
    <form
      class="flex flex-wrap items-end gap-3 border-t border-base-300 px-5 py-4"
      aria-label="Retirement"
      onsubmit={(event) => {
        event.preventDefault();
        void saveRetirement();
      }}
    >
      <label class="block"
        ><span class="trellis-field-label">Retire offline instances</span
        ><select class="select select-sm" bind:value={retirement}
          ><option value="default"
            >Like other services ({policy.installation.retireOffline
              ? `after ${Math.round(policy.installation.retireOfflineAfterSeconds / 3600)} hours`
              : "never"})</option
          ><option value="after">After a set time</option><option value="off"
            >Never</option
          ></select
        ></label
      >
      {#if retirement === "after"}<label class="block"
          ><span class="trellis-field-label">Hours offline</span><input
            class="input input-sm w-24"
            type="number"
            min="1"
            bind:value={retirementHours}
          /></label
        >{/if}
      <label class="block"
        ><span class="trellis-field-label">Added by hand too</span><select
          class="select select-sm"
          bind:value={provisioned}
          ><option value="default"
            >Like other services ({policy.installation.retireProvisioned
              ? "yes"
              : "no"})</option
          ><option value="on">Yes</option><option value="off">No</option
          ></select
        ></label
      >
      <button class="btn btn-outline btn-sm" type="submit" disabled={busy}
        >Save</button
      >
    </form>
  {/if}
</section>
