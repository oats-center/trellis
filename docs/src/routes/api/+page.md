---
title: "API reference"
description: "Generated TypeScript and Rust API docs for the public Trellis libraries."
sidebar: false
---

<script>
  import { asset } from '$app/paths'

  // Generated reference sites are static files, not app routes, so they load with a full page.
  const typescript = [
    { name: '@oatscenter/trellis', href: '/api/typescript/modules/_oatscenter_trellis.html', about: 'Clients, generated SDKs, and service APIs.' },
    { name: '@oatscenter/trellis-svelte', href: '/api/typescript/modules/_oatscenter_trellis-svelte.html', about: 'Svelte integration for browser apps.' },
    { name: '@oatscenter/trellis-testkit', href: '/api/typescript/modules/_oatscenter_trellis-testkit.html', about: 'Integration test helpers for service repositories.' },
    { name: '@oatscenter/result', href: '/api/typescript/modules/_oatscenter_result.html', about: 'Result and AsyncResult for explicit error handling.' },
  ]
  const rust = [
    { name: 'trellis-rs', href: '/api/rust/trellis_rs/index.html', about: 'The Rust runtime and Trellis-owned SDK modules.' },
    { name: 'trellis-protocol', href: '/api/rust/trellis_protocol/index.html', about: 'Artifacts, permissions, and signed authorization primitives.' },
    { name: 'trellis-testkit', href: '/api/rust/trellis_testkit/index.html', about: 'Out-of-process live-test harness for Rust services and apps.' },
  ]
</script>

Use the [library guides](/guides/libraries/) for how to use Trellis from TypeScript and Rust. Use this reference when you need exact types and function signatures.

## TypeScript

<ul>
{#each typescript as lib}
  <li><a href={asset(lib.href)} data-sveltekit-reload><code>{lib.name}</code></a>: {lib.about}</li>
{/each}
</ul>

## Rust

<ul>
{#each rust as lib}
  <li><a href={asset(lib.href)} data-sveltekit-reload><code>{lib.name}</code></a>: {lib.about}</li>
{/each}
</ul>
