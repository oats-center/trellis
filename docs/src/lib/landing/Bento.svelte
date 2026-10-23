<script>
  import { resolve } from '$app/paths'
  import { IconifyIcon } from '@sveltepress/theme-default/components'

  const tiles = [
    {
      icon: 'file-code',
      title: 'Contracts and generated SDKs',
      body: 'Describe requests, events, live views, and jobs once in Trellis IDL. Trellis checks the contract and generates typed TypeScript and Rust SDKs, so callers and services agree on every payload.',
      to: '/guides/concepts/contracts',
      size: 'wide',
    },
    {
      icon: 'shield-lock',
      title: 'Permissions people approve',
      body: 'Every call needs a capability the contract declares. People choose what an app may do when they sign in, and administrators decide what services may use.',
      to: '/guides/concepts/authentication-and-authorization',
    },
    {
      icon: 'bolt',
      title: 'Events that reach their listeners',
      body: 'Published events are kept and delivered to each listening service. Failed deliveries wait for someone to retry or dismiss them instead of disappearing.',
      to: '/guides/write-a-service/publish-events',
    },
    {
      icon: 'list-check',
      title: 'Background jobs',
      body: 'Services queue work that runs with retries, progress, and a history you can inspect. Operators see what failed and run it again.',
      to: '/guides/concepts/jobs',
    },
    {
      icon: 'devices',
      title: 'Devices and apps, not only services',
      body: 'Browser apps sign people in. Devices activate with a code an administrator approves. Command-line tools log in through the same flow.',
      to: '/guides/concepts/devices-and-activation',
    },
    {
      icon: 'plug-connected',
      title: 'One connection for every kind of call',
      body: 'Requests, events, live updates, long-running operations, and file transfers share one authenticated connection, with the same errors and the same tracing.',
      to: '/guides/concepts/communication',
      size: 'full',
    },
  ]
</script>

<div class="bento">
  {#each tiles as t}
    <a class="tile {t.size ?? ''}" href={resolve(t.to)}>
      <span class="icon"><IconifyIcon collection="tabler" name={t.icon} /></span>
      <h3>{t.title}</h3>
      <p>{t.body}</p>
    </a>
  {/each}
</div>

<style>
  .bento {
    display: grid;
    gap: 1rem;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
  }
  @media (min-width: 960px) {
    .bento { grid-template-columns: repeat(3, 1fr); grid-auto-flow: dense; }
    .wide { grid-column: span 2; }
    .full { grid-column: 1 / -1; }
  }
  .tile {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 1.5rem;
    border-radius: 1rem;
    text-decoration: none;
    color: inherit;
    background: color-mix(in srgb, var(--brand) 5%, transparent);
    border: 1px solid color-mix(in srgb, var(--brand) 18%, transparent);
    transition: border-color 0.2s, background 0.2s;
    overflow: hidden;
  }
  .tile:hover {
    border-color: var(--brand);
    background: color-mix(in srgb, var(--brand) 9%, transparent);
  }
  .tile::after {
    content: '→';
    position: absolute;
    top: 1.3rem;
    right: 1.4rem;
    opacity: 0;
    color: var(--warm);
    transition: opacity 0.2s, transform 0.2s;
  }
  .tile:hover::after { opacity: 1; transform: translateX(3px); }
  .full {
    background:
      linear-gradient(160deg, color-mix(in srgb, var(--warm) 14%, transparent), transparent 60%),
      color-mix(in srgb, var(--brand) 5%, transparent);
  }
  .icon {
    display: inline-flex;
    align-self: flex-start;
    padding: 0.6rem;
    border-radius: 0.7rem;
    font-size: 1.4rem;
    color: var(--on-brand);
    background: var(--brand);
  }
  h3 { margin: 0.8rem 0 0; font-size: 1.2rem; font-weight: 650; }
  p { margin: 0; line-height: 1.6; font-size: 0.95rem; color: color-mix(in srgb, currentColor 70%, transparent); }
</style>
