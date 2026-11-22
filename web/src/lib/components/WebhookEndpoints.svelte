<script lang="ts">
  import { apis } from "$lib/trellis-apis";
  import { codecs } from "@oatscenter/trellis/generated";
  import { ulid } from "ulid";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { usesGeneratedTls } from "$lib/config";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import {
    parseWebhookSetting,
    showWebhookSetting,
    type WebhookSettingUnit,
  } from "$lib/console/webhook_settings.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import Panel from "$lib/components/Panel.svelte";

  type Endpoint = apis.webhooks.EndpointsListOutput["endpoints"][number];
  type Attempt = apis.webhooks.AttemptsQueryOutput["attempts"][number];
  type Setting = NonNullable<apis.auth.WebhooksUpdateInput["setting"]>;

  // The Webhooks section of a service's page: each endpoint's URL, how it
  // checks requests, its secrets and runtime settings, and recent requests.
  let { deploymentId }: { deploymentId: string } = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("webhook-endpoints");
  const manage = $derived(canPerform(authority.authority, "webhooksUpdate"));
  const generatedTls = usesGeneratedTls();

  let endpoints = $state.raw<Endpoint[]>([]);
  let attempts = $state.raw<Record<string, Attempt[]>>({});
  let loadError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  // Settings shown and edited, with the unit people type them in.
  const SETTINGS: readonly {
    setting: Setting;
    field: keyof Endpoint["settings"];
    label: string;
    unit: WebhookSettingUnit;
  }[] = [
    {
      setting: "requestsPerSecond",
      field: "requestsPerSecond",
      label: "Requests per second",
      unit: "count",
    },
    { setting: "burst", field: "burst", label: "Burst", unit: "count" },
    {
      setting: "failuresPerMinute",
      field: "failuresPerMinute",
      label: "Failed requests per address per minute",
      unit: "count",
    },
    {
      setting: "maxBody",
      field: "maxBodyBytes",
      label: "Body size limit",
      unit: "bytes",
    },
    {
      setting: "dedupeWindow",
      field: "dedupeWindowMs",
      label: "Dedupe window",
      unit: "ms",
    },
    {
      setting: "rotationOverlap",
      field: "rotationOverlapMs",
      label: "Old secret accepted for",
      unit: "ms",
    },
    {
      setting: "recordRetention",
      field: "recordRetentionMs",
      label: "Keep request records",
      unit: "ms",
    },
    {
      setting: "intakeRetention",
      field: "intakeRetentionMs",
      label: "Keep stored requests",
      unit: "ms",
    },
  ];

  $effect(() => {
    void deploymentId;
    void load();
  });

  async function load(): Promise<void> {
    const token = scope.begin();
    try {
      const listed = await trellis
        .endpointsList(
          apis.webhooks.API.actions["rpc:Endpoints.List"].input.decode({
            service: deploymentId,
          }),
        )
        .orThrow();
      const recent: Record<string, Attempt[]> = {};
      if (canPerform(authority.authority, "webhooksAttemptsQuery")) {
        await Promise.all(
          listed.endpoints.map(async (endpoint) => {
            const found = await trellis
              .attemptsQuery(
                apis.webhooks.API.actions["rpc:Attempts.Query"].input.decode({
                  service: deploymentId,
                  endpoint: endpoint.endpoint,
                  limit: 10,
                }),
              )
              .orThrow();
            recent[endpoint.endpoint] = [...found.attempts];
          }),
        );
      }
      if (!scope.isCurrent(token)) return;
      endpoints = [...listed.endpoints];
      attempts = recent;
      loadError = null;
    } catch (cause) {
      if (scope.isCurrent(token))
        loadError = projectConsoleError(cause).message;
    }
  }

  async function update(
    endpoint: string,
    change: "enable" | "disable" | "rotateUrl" | "retireUrl" | "set",
    setting?: Setting,
    value?: bigint | null,
  ): Promise<void> {
    busy = true;
    error = null;
    try {
      await trellis
        .webhooksUpdate(
          apis.auth.API.actions["rpc:Webhooks.Update"].input.decode({
            deploymentId,
            endpoint,
            change,
            ...(setting ? { setting } : {}),
            ...(value === undefined
              ? {}
              : { value: value === null ? null : value.toString() }),
            idempotencyKey: codecs.ulid.decode(ulid()).toString(),
          }),
        )
        .orThrow();
      await load();
    } catch (cause) {
      const refused = classifyMutationError(cause).kind !== "unknown";
      error = refused
        ? projectConsoleError(cause).message
        : "We couldn't confirm the change. Refresh before trying again.";
      // A refusal means the endpoint changed since it was read; show it as
      // it is now.
      if (refused) await load();
    } finally {
      busy = false;
    }
  }

  async function confirmDisable(endpoint: Endpoint): Promise<void> {
    const confirmed = await confirmationModal?.confirm({
      title: `Disable ${endpoint.title}?`,
      message:
        "Its URL answers 503 and stores nothing until you enable it again.",
      confirmLabel: "Disable webhook",
    });
    if (confirmed) await update(endpoint.endpoint, "disable");
  }

  // The setting being edited, by endpoint and setting, and what was typed.
  let editing = $state<{ key: string; text: string } | null>(null);

  async function saveSetting(
    endpoint: Endpoint,
    entry: (typeof SETTINGS)[number],
  ): Promise<void> {
    const text = editing?.text ?? "";
    editing = null;
    if (text.trim() === "")
      return update(endpoint.endpoint, "set", entry.setting, null);
    const value = parseWebhookSetting(text, entry.unit);
    if (value === null) {
      error = `${entry.label} takes ${
        {
          count: "a number, such as 50",
          ms: "a duration, such as 30s, 5m, or 7d",
          bytes: "a size, such as 512KiB or 16MiB",
        }[entry.unit]
      }.`;
      return;
    }
    await update(endpoint.endpoint, "set", entry.setting, value);
  }
</script>

{#if loadError}
  <Notice variant="error">{loadError}</Notice>
{:else if endpoints.length > 0}
  <Panel title="Webhooks">
    {#if error}<Notice variant="error">{error}</Notice>{/if}
    <ul class="flex flex-col gap-6" aria-label="Webhook endpoints">
      {#each endpoints as endpoint (endpoint.endpoint)}
        <li class="flex flex-col gap-3" aria-label={endpoint.title}>
          <div class="flex flex-wrap items-center justify-between gap-2">
            <div class="min-w-0">
              <h3 class="font-semibold">{endpoint.title}</h3>
              <p class="text-sm text-base-content/65">{endpoint.description}</p>
            </div>
            <div class="flex items-center gap-2">
              <span
                class={[
                  "badge badge-soft",
                  endpoint.enabled ? "badge-success" : "badge-warning",
                ]}>{endpoint.enabled ? "Enabled" : "Disabled"}</span
              >
              {#if manage}
                <ActionMenu
                  widthClass="w-48"
                  ariaLabel={`Actions for ${endpoint.title}`}
                >
                  <li>
                    {#if endpoint.enabled}
                      <button
                        class="text-error"
                        disabled={busy}
                        onclick={() => void confirmDisable(endpoint)}
                        >Disable webhook</button
                      >
                    {:else}
                      <button
                        disabled={busy}
                        onclick={() => void update(endpoint.endpoint, "enable")}
                        >Enable webhook</button
                      >
                    {/if}
                  </li>
                  <li>
                    <button
                      disabled={busy}
                      onclick={() =>
                        void update(endpoint.endpoint, "rotateUrl")}
                      >New URL</button
                    >
                  </li>
                  {#if endpoint.previousUrls.length > 0}
                    <li>
                      <button
                        class="text-error"
                        disabled={busy}
                        onclick={() =>
                          void update(endpoint.endpoint, "retireUrl")}
                        >Retire earlier URLs</button
                      >
                    </li>
                  {/if}
                </ActionMenu>
              {/if}
            </div>
          </div>
          <div class="flex min-w-0 items-center gap-2">
            <code class="min-w-0 truncate text-sm" title={endpoint.url}
              >{endpoint.url}</code
            >
            <CopyButton
              value={endpoint.url}
              label={`Copy the URL of ${endpoint.title}`}
            />
          </div>
          {#if generatedTls}
            <Notice variant="warning"
              >Senders on the internet won't trust this Trellis's generated
              certificate.</Notice
            >
          {/if}
          {#if endpoint.unauthenticated}
            <Notice variant="warning"
              >This endpoint checks nothing: anyone with the URL can send it
              requests.</Notice
            >
          {/if}
          {#if endpoint.previousUrls.length > 0}
            <p class="text-sm text-base-content/65">
              Earlier URLs still work until you retire them: {endpoint.previousUrls.join(
                ", ",
              )}
            </p>
          {/if}
          <dl
            class="grid grid-cols-1 gap-x-6 gap-y-2 text-sm sm:grid-cols-[12rem_1fr]"
          >
            <dt class="text-base-content/65">Checks</dt>
            <dd>
              <ul>
                {#each endpoint.verifiers as verifier, index (index)}<li>
                    {verifier}
                  </li>{/each}
                {#if endpoint.decides}<li>The service decides</li>{/if}
              </ul>
            </dd>
            <dt class="text-base-content/65">Answers</dt>
            <dd>
              {endpoint.mode === "sync"
                ? "After the request is checked and recorded"
                : "At once, checking afterwards"}
            </dd>
            {#each endpoint.secrets as secret (secret.name)}
              <dt class="text-base-content/65">{secret.name}</dt>
              <dd>
                {secret.isSet ? "Set" : "Not set"}
                {#if secret.currentLastMatchedAt}
                  · last matched {formatDate(
                    String(secret.currentLastMatchedAt),
                  )}
                {/if}
                {#if secret.previousAcceptedUntil}
                  · old value accepted until {formatDate(
                    String(secret.previousAcceptedUntil),
                  )}
                {/if}
                {#if secret.previousLastMatchedAt}
                  · old value last matched {formatDate(
                    String(secret.previousLastMatchedAt),
                  )}
                {/if}
              </dd>
            {/each}
            {#each SETTINGS as entry (entry.setting)}
              <dt class="text-base-content/65">{entry.label}</dt>
              <dd class="flex items-center gap-2">
                {#if editing?.key === `${endpoint.endpoint}:${entry.setting}`}
                  <form
                    class="flex items-center gap-2"
                    onsubmit={(event) => {
                      event.preventDefault();
                      void saveSetting(endpoint, entry);
                    }}
                  >
                    <input
                      class="input input-sm w-32"
                      aria-label={`${entry.label} of ${endpoint.title}`}
                      placeholder="Default"
                      bind:value={editing.text}
                    />
                    <button
                      type="submit"
                      class="btn btn-primary btn-xs"
                      disabled={busy}>Save</button
                    >
                    <button
                      type="button"
                      class="btn btn-ghost btn-xs"
                      onclick={() => (editing = null)}>Cancel</button
                    >
                  </form>
                {:else}
                  <span
                    >{showWebhookSetting(
                      endpoint.settings[entry.field],
                      entry.unit,
                    )}</span
                  >
                  {#if manage}
                    <button
                      type="button"
                      class="btn btn-ghost btn-xs"
                      disabled={busy}
                      aria-label={`Change ${entry.label} of ${endpoint.title}`}
                      onclick={() =>
                        (editing = {
                          key: `${endpoint.endpoint}:${entry.setting}`,
                          text: showWebhookSetting(
                            endpoint.settings[entry.field],
                            entry.unit,
                          ),
                        })}>Change</button
                    >
                  {/if}
                {/if}
              </dd>
            {/each}
            <dt class="text-base-content/65">Last 24 hours</dt>
            <dd>
              {#if endpoint.recent.length === 0}
                No requests
              {:else}
                {endpoint.recent
                  .map((entry) => `${entry.count} ${entry.outcome}`)
                  .join(" · ")}
              {/if}
            </dd>
          </dl>
          {#if (attempts[endpoint.endpoint] ?? []).length > 0}
            <table
              class="table table-sm"
              aria-label={`Recent requests to ${endpoint.title}`}
            >
              <thead>
                <tr
                  ><th class="trellis-primary">Received</th><th>Outcome</th><th
                    class="hidden sm:table-cell">Status</th
                  ><th class="hidden sm:table-cell">Reason</th></tr
                >
              </thead>
              <tbody>
                {#each attempts[endpoint.endpoint] ?? [] as attempt (attempt.attemptId)}
                  <tr>
                    <td class="trellis-primary"
                      >{formatDate(String(attempt.receivedAt))}</td
                    >
                    <td>{attempt.outcome}</td>
                    <td class="hidden sm:table-cell">{attempt.status ?? ""}</td>
                    <td class="hidden sm:table-cell">{attempt.reason ?? ""}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </li>
      {/each}
    </ul>
  </Panel>
{/if}
<ConfirmationModal bind:this={confirmationModal} />
