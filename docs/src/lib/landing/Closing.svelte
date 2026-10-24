<script>
  import { asset, resolve } from '$app/paths'
  import FamilyFooter from '#lib/family/FamilyFooter.svelte'
</script>

<section class="cta on-ink">
  <div class="inner">
    <h2>Next steps</h2>
    <p>The tutorial builds an orders service from an empty folder: a contract, a generated SDK, handlers, events, and a test.</p>
    <div class="actions">
      <a class="btn primary" href={resolve('/guides/write-a-service')}>Write a service →</a>
      <a class="btn" href={resolve('/guides/concepts')}>Learn the concepts</a>
    </div>
  </div>
</section>

<FamilyFooter
  current="trellis"
  logo={asset('/logo.svg')}
  links={[
    { label: 'Guides', href: resolve('/guides/overview') },
    { label: 'Concepts', href: resolve('/guides/concepts') },
    { label: 'Design', href: resolve('/design') },
    { label: 'API', href: resolve('/api') },
    { label: 'GitHub', href: 'https://github.com/oats-center/trellis' },
  ]}
/>

<style>
  .cta { max-width: 76rem; margin: 6rem auto 0; padding: 0 1.5rem; }
  .inner {
    position: relative;
    padding: 3.5rem 2rem;
    border-radius: 1.25rem;
    text-align: center;
    color: var(--on-ink);
    background:
      repeating-linear-gradient(45deg, transparent 0 27px, rgb(255 255 255 / 0.035) 27px 28px),
      repeating-linear-gradient(-45deg, transparent 0 27px, rgb(255 255 255 / 0.035) 27px 28px),
      radial-gradient(80% 120% at 85% 0%, color-mix(in srgb, var(--warm) 35%, transparent), transparent 60%),
      var(--ink);
    overflow: hidden;
  }
  h2 { margin: 0; font-size: clamp(1.7rem, 3.5vw, 2.4rem); letter-spacing: -0.02em; border: 0; padding: 0; color: #fff; }
  p { margin: 0.8rem auto 2rem; max-width: 34rem; color: var(--on-ink-muted); font-size: 1.05rem; }
  .actions { display: flex; flex-wrap: wrap; justify-content: center; gap: 0.75rem; }
  .btn {
    padding: 0.7rem 1.3rem;
    border-radius: 0.6rem;
    font-weight: 600;
    text-decoration: none;
    color: var(--on-ink);
    border: 1.5px solid rgb(255 255 255 / 0.25);
  }
  .btn:hover { border-color: var(--warm); }
  .btn.primary { background: var(--warm); border-color: var(--warm); color: var(--warm-ink); }
  .btn.primary:hover { background: color-mix(in srgb, var(--warm) 85%, white); }

</style>
