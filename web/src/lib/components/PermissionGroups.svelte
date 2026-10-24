<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import Icon from "./Icon.svelte";
  import { groupByArea } from "$lib/console/capability_areas.ts";

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

  // People pick access by API area; a capability group sits in its area.
  const groups = $derived(
    groupByArea(capabilities, (capability) => capability.identity.capabilityId),
  );

  function ids(group: { items: Capability[] }): string[] {
    return group.items.map((item) => item.identity.capabilityId);
  }

  function setGroup(group: { items: Capability[] }, on: boolean): void {
    const members = ids(group);
    selected = on
      ? [...new Set([...selected, ...members])]
      : selected.filter((id) => !members.includes(id));
  }

  function indeterminate(node: HTMLInputElement, value: boolean) {
    node.indeterminate = value;
    return {
      update(next: boolean) {
        node.indeterminate = next;
      },
    };
  }
</script>

<ul class="divide-y divide-base-300 rounded-box border border-base-300">
  {#each groups as group (group.id)}
    {@const count = ids(group).filter((id) => selected.includes(id)).length}
    {@const open = expanded.includes(group.id)}
    <li role="group" aria-label={group.title}>
      <div class="flex items-center gap-3 pl-4">
        <input
          type="checkbox"
          class="checkbox checkbox-sm"
          aria-label={`All of ${group.title}`}
          checked={count === group.items.length}
          use:indeterminate={count > 0 && count < group.items.length}
          {disabled}
          onchange={(event) => setGroup(group, event.currentTarget.checked)}
        />
        <button
          type="button"
          class="flex flex-1 items-center gap-3 py-3 pr-4 text-left"
          aria-expanded={open}
          onclick={() =>
            (expanded = open
              ? expanded.filter((id) => id !== group.id)
              : [...expanded, group.id])}
        >
          <span class="flex-1">
            <span class="block font-medium">{group.title}</span>
            <span class="block text-xs text-base-content/60"
              >{group.description}</span
            >
          </span>
          <span class="text-sm text-base-content/60 tabular-nums"
            >{count} of {group.items.length}</span
          >
          <Icon
            name="chevronDown"
            size={16}
            class={`text-base-content/50 transition-transform ${open ? "rotate-180" : ""}`}
          />
        </button>
      </div>
      {#if open}
        <ul class="border-t border-base-300 bg-base-200/40 py-1">
          {#each group.items as item (item.identity.capabilityId)}
            <li>
              <label
                class="flex cursor-pointer items-start gap-3 py-2 pl-11 pr-4"
              >
                <input
                  type="checkbox"
                  class="checkbox checkbox-sm mt-0.5"
                  checked={selected.includes(item.identity.capabilityId)}
                  {disabled}
                  onchange={(event) => {
                    const id = item.identity.capabilityId;
                    selected = event.currentTarget.checked
                      ? [...selected, id]
                      : selected.filter((value) => value !== id);
                  }}
                />
                <span>
                  <span class="block text-sm font-medium">{item.title}</span>
                  <span class="block text-xs text-base-content/60"
                    >{item.description}</span
                  >
                  {#if item.kind === "group"}<span
                      class="block text-xs text-base-content/60"
                      >Includes {item.members.length}
                      {item.members.length === 1
                        ? "permission"
                        : "permissions"} in this area</span
                    >{/if}
                </span>
              </label>
            </li>
          {/each}
        </ul>
      {/if}
    </li>
  {/each}
</ul>
