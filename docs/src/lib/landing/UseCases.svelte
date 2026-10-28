<script>
  import { resolve } from '$app/paths'
  import { IconifyIcon } from '@sveltepress/theme-default/components'

  const cases = [
    { icon: 'server', name: 'Backend services', to: '/guides/write-a-service', body: 'Services that answer requests, publish events, and run background jobs, written in TypeScript or Rust against a generated SDK.', tags: ['TypeScript', 'Rust', 'jobs'] },
    { icon: 'browser', name: 'Web apps and portals', to: '/guides/writing-sveltekit-apps', body: 'Browser apps sign people in, ask only for the access they need, and call services directly with typed clients.', tags: ['SvelteKit', 'sign-in', 'consent'] },
    { icon: 'cpu', name: 'Devices', to: '/guides/devices', body: 'Field devices activate with a code an administrator approves, then publish readings and receive commands over the same connection.', tags: ['activation', 'offline', 'events'] },
    { icon: 'terminal-2', name: 'Tools and automation', to: '/guides/install-trellis', body: 'Command-line tools and agents log in through the device flow and use the same APIs as everything else.', tags: ['CLI', 'device login', 'AI agents'] },
  ]
</script>

<div class="cases">
  {#each cases as c}
    <a class="card" href={resolve(c.to)}>
      <span class="icon"><IconifyIcon collection="tabler" name={c.icon} /></span>
      <h3>{c.name}</h3>
      <p>{c.body}</p>
      <ul>{#each c.tags as t}<li>{t}</li>{/each}</ul>
    </a>
  {/each}
</div>

<style>
  .cases { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(17rem, 1fr)); }
  .card {
    display: flex;
    flex-direction: column;
    padding: 1.5rem;
    border-radius: 1rem;
    text-decoration: none;
    color: inherit;
    border: 1px solid color-mix(in srgb, currentColor 12%, transparent);
    transition: border-color 0.2s, transform 0.2s;
  }
  .card:hover { border-color: var(--warm); transform: translateY(-3px); }
  .icon { font-size: 2rem; color: var(--warm); }
  h3 { margin: 1rem 0 0.4rem; font-size: 1.3rem; }
  p { margin: 0; flex: 1; line-height: 1.6; font-size: 0.95rem; color: color-mix(in srgb, currentColor 70%, transparent); }
  ul { list-style: none; margin: 1.2rem 0 0; padding: 0; display: flex; flex-wrap: wrap; gap: 0.4rem; }
  li {
    padding: 0.2rem 0.6rem;
    border-radius: 999px;
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--brand);
    background: color-mix(in srgb, var(--brand) 12%, transparent);
  }
</style>
