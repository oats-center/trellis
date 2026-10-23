<script>
  import { resolve } from '$app/paths'

  // Real output from `trellis check` on the tutorial project (docs/examples/orders).
  const views = [
    {
      id: 'check',
      lines: [
        ['cmd', 'trellis check'],
        ['pass', 'Checked acme-orders 1.0.0'],
        ['blank'],
        ['note', '# The contract parses, resolves, and validates.'],
        ['note', '# `trellis generate` then writes the typed SDK.'],
      ],
    },
    {
      id: 'mistake',
      lines: [
        ['cmd', 'trellis check'],
        ['err', "Error:   × selected action 'rpc Cancel' does not exist"],
        ['dim', '    ╭─[contract.trellis:4:1]'],
        ['dim', '  4 │ ╭─▶ api orders@v1 {'],
        ['dim', '  5 │ │     title "Orders";'],
        ['dim', '  7 │ │     rpc Create { input CreateOrderRequest; ... }'],
        ['dim', ' 14 │ │         allows { rpc Cancel; }'],
        ['dim', ' 17 │ ╰─▶ }'],
      ],
    },
  ]

  let active = $state(0)
  const v = $derived(views[active])
</script>

<section class="hero">
  <div class="field" aria-hidden="true"></div>

  <div class="copy">
    <p class="kicker"><span>contracts</span> + <span>permissions</span> + <span>events</span> + <span>jobs</span></p>
    <h1>Trellis</h1>
    <p class="tagline">Build connected services without building the plumbing.</p>
    <p class="lead">
      Each program says what it provides and what it needs in a contract. Trellis generates typed
      TypeScript and Rust SDKs from it, checks every call against the permissions people approved,
      and carries requests, events, live views, and background jobs between services, web apps,
      command-line tools, and devices.
    </p>
    <div class="actions">
      <a class="btn primary" href={resolve('/guides/write-a-service')}>Write a service <span aria-hidden="true">→</span></a>
      <a class="btn" href={resolve('/guides/overview')}>Read the docs</a>
      <a class="btn ghost" href="https://github.com/oats-center/trellis">GitHub</a>
    </div>
  </div>

  <div class="console on-ink">
    <div class="tabs" role="tablist" aria-label="Example output">
      {#each views as view, i}
        <button role="tab" aria-selected={i === active} class:on={i === active} onclick={() => (active = i)}>
          {view.id}
        </button>
      {/each}
    </div>
    <pre aria-live="polite">{#each v.lines as l}{#if l[0] === 'cmd'}<span class="p">$</span> {l[1]}
{:else if l[0] === 'kv'}{l[1]}<b>{l[2]}</b>
{:else if l[0] === 'log' || l[0] === 'ok'}<span class="lvl">{l[1]}</span> <span class={l[0] === 'ok' ? 'ok' : ''}>{l[2]}</span> <span class="dim">{l[3]}</span>
{:else if l[0] === 'pass'}<span class="ok">{l[1]}</span>
{:else if l[0] === 'err'}<span class="err">{l[1]}</span>
{:else if l[0] === 'note'}<span class="note">{l[1]}</span>
{:else if l[0] === 'blank'}{'\n'}{:else}<span class="dim">{l[1]}</span>
{/if}{/each}</pre>
    <p class="caption">Real output from the tutorial project.</p>
  </div>
</section>

<style>
  .hero {
    position: relative;
    display: grid;
    gap: 3rem;
    align-items: center;
    max-width: 76rem;
    margin: 0 auto;
    padding: 4.5rem 1.5rem 3rem;
  }
  @media (min-width: 960px) {
    .hero { grid-template-columns: 1.05fr 1fr; padding-top: 6rem; }
  }

  /* Trellis motif: a diamond lattice the leaves climb */
  .field {
    position: absolute;
    inset: -2rem -50vw 0;
    z-index: -1;
    background:
      radial-gradient(60% 70% at 75% 30%, color-mix(in srgb, var(--warm) 14%, transparent), transparent 70%),
      repeating-linear-gradient(45deg, transparent 0 27px, color-mix(in srgb, var(--brand) 8%, transparent) 27px 28px),
      repeating-linear-gradient(-45deg, transparent 0 27px, color-mix(in srgb, var(--brand) 8%, transparent) 27px 28px);
    mask-image: linear-gradient(to bottom, black 40%, transparent);
  }

  .kicker {
    margin: 0 0 1rem;
    font-family: var(--svp-code-font);
    font-size: 0.8rem;
    color: color-mix(in srgb, currentColor 55%, transparent);
  }
  .kicker span { color: var(--brand); font-weight: 600; }

  h1 {
    margin: 0;
    font-size: clamp(2.4rem, 5vw, 3.6rem);
    line-height: 1;
    font-weight: 750;
    letter-spacing: -0.035em;
  }
  .tagline {
    margin: 0.6rem 0 0;
    font-family: var(--font-display);
    font-size: clamp(1.25rem, 2.2vw, 1.6rem);
    font-weight: 600;
    line-height: 1.25;
    color: color-mix(in srgb, currentColor 80%, transparent);
  }

  .lead {
    margin: 1.5rem 0 0;
    max-width: 36rem;
    font-size: 1.15rem;
    line-height: 1.65;
    color: color-mix(in srgb, currentColor 72%, transparent);
  }

  .actions { display: flex; flex-wrap: wrap; gap: 0.75rem; margin-top: 2rem; }
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.7rem 1.3rem;
    border-radius: 0.6rem;
    font-weight: 600;
    text-decoration: none;
    color: inherit;
    border: 1.5px solid color-mix(in srgb, currentColor 22%, transparent);
    transition: transform 0.15s, border-color 0.15s, background 0.15s;
  }
  .btn:hover { transform: translateY(-1px); border-color: var(--brand); }
  .btn.primary { background: var(--brand); border-color: var(--brand); color: var(--on-brand); }
  .btn.primary:hover { background: var(--brand-deep); }
  .btn.ghost { border-color: transparent; }

  .console {
    min-width: 0;
    border-radius: 1rem;
    background: var(--console-bg);
    border: 1px solid var(--console-line);
    box-shadow: 0 40px 80px -30px rgb(30 22 10 / 0.55);
    overflow: hidden;
  }
  .tabs { display: flex; background: var(--console-bar); border-bottom: 1px solid var(--console-line); }
  .tabs button {
    flex: 1;
    padding: 0.65rem 0.5rem;
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    color: var(--console-dim);
    font: 500 0.78rem var(--svp-code-font);
    cursor: pointer;
  }
  .tabs button.on { color: var(--on-ink); border-bottom-color: #c2a86e; background: var(--console-bg); }
  pre {
    margin: 0;
    padding: 1.1rem 1.25rem 0.5rem;
    min-height: 15.5rem;
    font: 0.78rem/1.65 var(--svp-code-font);
    color: var(--console-text);
    background: none;
    overflow-x: auto;
  }
  pre b { color: #fff; font-weight: 600; }
  .p { color: #c2a86e; }
  .lvl { color: #6fb3d0; }
  .ok { color: #6fd08c; }
  .err { color: #f08a7a; }
  .dim { color: var(--console-dim); }
  .note { color: #a8a070; }
  .caption {
    margin: 0;
    padding: 0.4rem 1.25rem 0.9rem;
    font-size: 0.75rem;
    color: var(--console-dim);
  }
</style>
