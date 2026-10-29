<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import Icon from "./Icon.svelte";
  import { capabilityArea } from "$lib/console/capability_areas.ts";

  type Capability = apis.auth.CapabilitiesListOutput["items"][number];

  type Props = {
    capabilities: readonly Capability[];
    selected: string[];
    disabled?: boolean;
  };

  let {
    capabilities,
    selected = $bindable(),
    disabled = false,
  }: Props = $props();
  let expanded = $state<string[]>([]);

  // One section per API. Its groups are the usual choice; single
  // permissions wait under "More" for one-off needs.
  const sections = $derived.by(() => {
    const byApi = new Map<
      string,
      {
        id: string;
        title: string;
        groups: Capability[];
        singles: Capability[];
      }
    >();
    for (const capability of capabilities) {
      const id = capability.identity.capabilityId;
      const api = id.slice(0, Math.max(0, id.indexOf(":")));
      const section = byApi.get(api) ?? {
        id: api,
        title: capabilityArea(id).api,
        groups: [],
        singles: [],
      };
      (capability.kind === "group" ? section.groups : section.singles).push(
        capability,
      );
      byApi.set(api, section);
    }
    const byTitle = (a: Capability, b: Capability) =>
      a.title.localeCompare(b.title);
    return [...byApi.values()]
      .map((section) => ({
        ...section,
        groups: section.groups.toSorted(byTitle),
        singles: section.singles.toSorted(
          (a, b) =>
            capabilityArea(a.identity.capabilityId).title.localeCompare(
              capabilityArea(b.identity.capabilityId).title,
            ) || byTitle(a, b),
        ),
      }))
      .sort((a, b) => a.title.localeCompare(b.title));
  });

  function toggle(id: string, on: boolean): void {
    selected = on
      ? [...selected, id]
      : selected.filter((value) => value !== id);
  }
</script>

{#snippet choice(item: Capability, inset: boolean)}
  <label
    class={[
      "flex cursor-pointer items-start gap-3 py-2 pr-4",
      inset ? "pl-11" : "pl-4",
    ]}
  >
    <input
      type="checkbox"
      class="checkbox checkbox-sm mt-0.5"
      checked={selected.includes(item.identity.capabilityId)}
      {disabled}
      onchange={(event) =>
        toggle(item.identity.capabilityId, event.currentTarget.checked)}
    />
    <span>
      <span class="block text-sm font-medium">{item.title}</span>
      <span class="block text-xs text-base-content/60">{item.description}</span>
      {#if item.kind === "group"}<span
          class="block text-xs text-base-content/60"
          >Includes {item.members.length}
          {item.members.length === 1 ? "permission" : "permissions"}</span
        >{/if}
    </span>
  </label>
{/snippet}

<ul class="divide-y divide-base-300 rounded-box border border-base-300">
  {#each sections as section (section.id)}
    {@const count = [...section.groups, ...section.singles].filter((item) =>
      selected.includes(item.identity.capabilityId),
    ).length}
    {@const open = expanded.includes(section.id)}
    <li role="group" aria-label={section.title}>
      <div class="flex items-center justify-between px-4 pt-3 pb-1">
        <span class="font-semibold">{section.title}</span>
        <span class="text-sm text-base-content/60 tabular-nums"
          >{count} selected</span
        >
      </div>
      {#each section.groups as item (item.identity.capabilityId)}
        {@render choice(item, false)}
      {/each}
      {#if section.groups.length && section.singles.length}
        <button
          type="button"
          class="flex w-full items-center gap-2 px-4 py-2 text-left text-sm text-base-content/70 hover:text-base-content"
          aria-expanded={open}
          onclick={() =>
            (expanded = open
              ? expanded.filter((id) => id !== section.id)
              : [...expanded, section.id])}
        >
          <Icon
            name="chevronDown"
            size={14}
            class={`transition-transform ${open ? "rotate-180" : ""}`}
          />
          More: {section.singles.length} single permissions
        </button>
      {/if}
      {#if !section.groups.length || open}
        <div class={section.groups.length ? "bg-base-200/40 pb-1" : "pb-1"}>
          {#each section.singles as item (item.identity.capabilityId)}
            {@render choice(item, section.groups.length > 0)}
          {/each}
        </div>
      {/if}
    </li>
  {/each}
</ul>
