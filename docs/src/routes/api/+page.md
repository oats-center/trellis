---
title: "API reference"
description: "Where to find the TypeScript and Rust API docs for the public Trellis libraries."
sidebar: false
---

<script>
  const typescript = [
    { name: '@oatscenter/trellis', about: 'Clients, generated SDKs, and service APIs.' },
    { name: '@oatscenter/trellis-svelte', about: 'Svelte integration for browser apps.' },
    { name: '@oatscenter/trellis-testkit', about: 'Integration test helpers for service repositories.' },
    { name: '@oatscenter/result', about: 'Result and AsyncResult for explicit error handling.' },
  ]
  const rust = [
    { name: 'trellis-rs', about: 'The Rust runtime and Trellis-owned SDK modules.' },
    { name: 'trellis-protocol', about: 'Artifacts, permissions, and signed authorization primitives.' },
    { name: 'trellis-testkit', about: 'Out-of-process live-test harness for Rust services and apps.' },
  ]
</script>

Use the [library guides](/guides/libraries/) for how to use Trellis from TypeScript and Rust. Use this reference when you need exact types and function signatures.

## TypeScript

Each package ships its types with JSDoc on every public export, so your editor shows signatures and documentation as you type.

<ul>
{#each typescript as lib}
  <li><a href={`https://www.npmjs.com/package/${lib.name}`}><code>{lib.name}</code></a>: {lib.about}</li>
{/each}
</ul>

## Rust

Rustdoc for each published crate is on docs.rs. Run `cargo doc --open` in your project to read it offline for the exact versions you depend on.

<ul>
{#each rust as lib}
  <li><a href={`https://docs.rs/${lib.name}`}><code>{lib.name}</code></a>: {lib.about}</li>
{/each}
</ul>
