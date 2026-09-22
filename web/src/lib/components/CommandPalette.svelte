<script lang="ts">
  import { base } from "$lib/console_paths";
  import { goto } from "$app/navigation";
  import { tick } from "svelte";
  import { filterCommands, type CommandEntry } from "../commands.ts";

  type Props = {
    commands: CommandEntry[];
    open?: boolean;
  };

  let { commands, open = $bindable(false) }: Props = $props();

  let dialog: HTMLDialogElement | undefined = $state();
  let input: HTMLInputElement | undefined = $state();
  let query = $state("");
  let activeIndex = $state(0);

  const results = $derived(filterCommands(commands, query));
  const groups = $derived(
    results.reduce<{ title: string; rows: { entry: CommandEntry; index: number }[] }[]>((acc, entry) => {
      const index = results.indexOf(entry);
      const last = acc[acc.length - 1];
      if (last && last.title === entry.group) last.rows.push({ entry, index });
      else acc.push({ title: entry.group, rows: [{ entry, index }] });
      return acc;
    }, []),
  );

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) {
      query = "";
      activeIndex = 0;
      dialog.showModal();
      void tick().then(() => input?.focus());
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  $effect(() => {
    activeIndex = results.length > 0 ? Math.min(activeIndex, results.length - 1) : 0;
  });

  function navigate(entry: CommandEntry | undefined) {
    if (!entry) return;
    open = false;
    // Command hrefs are authored against the route registry; base keeps the
    // app mount point correct without fighting the typed-route overloads.
    void goto(new URL(`${base}${entry.href}${entry.query ? `?${entry.query}` : ""}`, document.baseURI));
  }

  function moveActive(delta: number) {
    if (results.length === 0) return;
    activeIndex = (activeIndex + delta + results.length) % results.length;
    document.getElementById(`palette-row-${activeIndex}`)?.scrollIntoView({ block: "nearest" });
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      moveActive(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      moveActive(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      navigate(results[activeIndex]);
    }
  }
</script>

<dialog
  bind:this={dialog}
  class="trellis-palette"
  aria-label="Command palette"
  onclick={(event) => {
    if (event.target === dialog) open = false;
  }}
  onclose={() => {
    open = false;
  }}
>
  <input
    bind:this={input}
    bind:value={query}
    class="trellis-palette-input"
    type="text"
    role="combobox"
    aria-expanded="true"
    aria-controls="palette-results"
    aria-label="Search pages and views"
    placeholder="Type a page or view name..."
    autocomplete="off"
    spellcheck="false"
    onkeydown={handleKeydown}
  />
  <ul id="palette-results" class="trellis-palette-list" role="listbox" aria-label="Commands">
    {#if results.length === 0}
      <li class="trellis-palette-empty" role="status">
        No commands match “{query}”. Try a page name like jobs or sessions.
      </li>
    {:else}
      {#each groups as group (group.title)}
        <li class="trellis-palette-group" role="presentation">{group.title}</li>
        {#each group.rows as { entry, index } (entry.id)}
          <li
            id={`palette-row-${index}`}
            role="option"
            aria-selected={index === activeIndex}
            class:active={index === activeIndex}
            onkeydown={handleKeydown}
            onmousemove={() => {
              activeIndex = index;
            }}
            onclick={() => navigate(entry)}
          >
            <span>{entry.label}</span>
            <kbd>{entry.breadcrumb}</kbd>
          </li>
        {/each}
      {/each}
    {/if}
  </ul>
  <p class="trellis-palette-foot">
    <kbd>↑</kbd><kbd>↓</kbd> select · <kbd>Enter</kbd> go · <kbd>Esc</kbd> close
  </p>
</dialog>
