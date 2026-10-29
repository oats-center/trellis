<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import Icon from "./Icon.svelte";
  import { capabilityArea } from "$lib/console/capability_areas.ts";

  type Capability = apis.auth.CapabilitiesListOutput["items"][number];

  type Props = {
    capabilities: readonly Capability[];
    selected: string[];
    /** What the person already has some other way; shown, not chosen. */
    held?: readonly string[];
    disabled?: boolean;
  };

  let {
    capabilities,
    selected = $bindable(),
    held = [],
    disabled = false,
  }: Props = $props();
  let expanded = $state<string[]>([]);

  // A chosen or held group brings its members along.
  const groupOf = $derived.by(() => {
    const groups = new Map<string, string>();
    for (const capability of capabilities)
      if (
        capability.kind === "group" &&
        (selected.includes(capability.identity.capabilityId) ||
          held.includes(capability.identity.capabilityId))
      )
        for (const member of capability.members)
          if (!groups.has(member)) groups.set(member, capability.title);
    return groups;
  });

  function status(item: Capability): {
    checked: boolean;
    fixed: boolean;
    note?: string;
  } {
    const id = item.identity.capabilityId;
    if (selected.includes(id)) return { checked: true, fixed: false };
    if (held.includes(id))
      return { checked: true, fixed: true, note: "Already has this" };
    const group = groupOf.get(id);
    if (group)
      return { checked: true, fixed: true, note: `Included in ${group}` };
    return { checked: false, fixed: false };
  }

  function included(item: Capability): number {
    return item.members.filter(
      (member) =>
        selected.includes(member) ||
        held.includes(member) ||
        groupOf.has(member),
    ).length;
  }

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

  function toggle(item: Capability, on: boolean): void {
    const id = item.identity.capabilityId;
    // Choosing a group makes its separately chosen members redundant.
    selected = on
      ? [...selected.filter((value) => !item.members.includes(value)), id]
      : selected.filter((value) => value !== id);
  }
</script>

{#snippet choice(item: Capability, inset: boolean)}
  {@const current = status(item)}
  {@const some = item.kind === "group" ? included(item) : 0}
  <label
    class={[
      "flex items-start gap-3 py-2 pr-4",
      current.fixed ? "cursor-default" : "cursor-pointer",
      inset ? "pl-11" : "pl-4",
    ]}
  >
    <input
      type="checkbox"
      class="checkbox checkbox-sm mt-0.5"
      checked={current.checked}
      indeterminate={!current.checked && some > 0}
      disabled={disabled || current.fixed}
      onchange={(event) => toggle(item, event.currentTarget.checked)}
    />
    <span>
      <span class="block text-sm font-medium">{item.title}</span>
      <span class="block text-xs text-base-content/60">{item.description}</span>
      {#if current.note}<span class="block text-xs text-base-content/60"
          >{current.note}</span
        >{:else if item.kind === "group"}<span
          class="block text-xs text-base-content/60"
          >{#if some && !current.checked}{some} of {item.members.length} already included{:else}Includes
            {item.members.length}
            {item.members.length === 1
              ? "permission"
              : "permissions"}{/if}</span
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
