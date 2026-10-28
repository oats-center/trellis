<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ClassValue } from "svelte/elements";
  import Icon from "./Icon.svelte";

  type MenuAlign = "start" | "end";

  type Props = {
    children: Snippet;
    summary?: Snippet;
    label?: string;
    class?: ClassValue;
    buttonBaseClass?: ClassValue;
    buttonClass?: ClassValue;
    menuClass?: ClassValue;
    widthClass?: ClassValue;
    align?: MenuAlign;
    ariaLabel?: string;
    dataActionMenu?: boolean;
  };

  let {
    children,
    summary,
    label,
    class: className,
    buttonBaseClass = "btn btn-ghost btn-sm btn-square",
    buttonClass,
    menuClass,
    widthClass = "w-48",
    align = "end",
    ariaLabel,
    dataActionMenu = false,
  }: Props = $props();

  const id = $props.id();
  const GAP = 6;
  let trigger = $state<HTMLButtonElement>();
  let menu = $state<HTMLUListElement>();
  let open = $state(false);

  // The menu lives in the top layer, so tables and scroll boxes never clip it.
  // It opens below the button, or above when there is no room below.
  function place(): void {
    if (!trigger || !menu) return;
    const anchor = trigger.getBoundingClientRect();
    const width = menu.offsetWidth;
    const height = menu.offsetHeight;
    const left =
      align === "end" ? anchor.right - width : Math.max(GAP, anchor.left);
    const below = anchor.bottom + GAP;
    const top =
      below + height > window.innerHeight - GAP &&
      anchor.top - GAP - height > GAP
        ? anchor.top - GAP - height
        : below;
    menu.style.left = `${Math.max(GAP, Math.min(left, window.innerWidth - width - GAP))}px`;
    menu.style.top = `${top}px`;
  }

  function close(): void {
    if (menu?.matches(":popover-open")) menu.hidePopover();
  }

  function onBeforeToggle(event: ToggleEvent): void {
    // Hidden until placed, so it never flashes at the default position.
    if (event.newState === "open" && menu) menu.style.visibility = "hidden";
  }

  function onToggle(event: ToggleEvent): void {
    open = event.newState === "open";
    if (!open || !menu) return;
    place();
    menu.style.visibility = "";
  }

  function onMenuClick(event: MouseEvent): void {
    const target = event.target as Element | null;
    if (target?.closest("a, button")) close();
  }

  $effect(() => {
    if (!open) return;
    // A moved anchor would leave the menu floating; close it instead.
    const options = { capture: true, passive: true };
    window.addEventListener("scroll", close, options);
    window.addEventListener("resize", close, options);
    return () => {
      window.removeEventListener("scroll", close, options);
      window.removeEventListener("resize", close, options);
    };
  });
</script>

<div class={["inline-block", className]}>
  <button
    bind:this={trigger}
    type="button"
    class={[buttonBaseClass, buttonClass]}
    popovertarget={id}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label={ariaLabel ?? (label ? undefined : "Actions")}
    data-action-menu={dataActionMenu ? "" : undefined}
  >
    {#if summary}
      {@render summary()}
    {:else if label}
      {label}
    {:else}
      <Icon name="more" size={18} />
    {/if}
  </button>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions (delegated from the links and buttons inside) -->
  <ul
    bind:this={menu}
    {id}
    popover="auto"
    onbeforetoggle={onBeforeToggle}
    ontoggle={onToggle}
    onclick={onMenuClick}
    class={[
      "menu trellis-dropdown-menu trellis-popover-menu",
      widthClass,
      menuClass,
    ]}
  >
    {@render children()}
  </ul>
</div>
