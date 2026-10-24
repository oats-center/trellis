# Trellis Console — DESIGN.md

The local design contract for the Console (`web/`) and the OAuth portal
(`portal/`). Read it before changing UI, layout, styling, or copy.

## Who we design for

System administrators and Trellis users doing focused work: checking whether
things are healthy, finding a user or device, approving access, fixing what is
broken. They know their organisation and their systems. They do not think in
Trellis internals: contract IDs, capability atoms, bindings, delegations,
generations, revisions, or ULIDs.

Before designing a page, answer:

1. Why did the person come here? What question or task brought them?
2. What do they need to see first to answer it?
3. What will they do next, and is that action one obvious step away?

Design the page around those answers, not around the shape of the API.

## Language

- Use plain words for things people recognise: **Users, Roles, Apps, Connected
  apps, Services, Devices, APIs, Sessions, Jobs, Events, Health.**
- Never show raw capability IDs (`trellis.auth@v1::capabilityGrantsGrant`),
  revisions, generations, digests, or internal state names in primary UI.
  Show the human title. Put exact identifiers in a details disclosure or a copy
  button when someone needs them for support or scripting.
- Show names, not IDs. When a record only has an ID, show a short, copyable
  form and work toward giving it a name.
- No explanatory prose blocks. A page gets at most one short subtitle. If a
  control needs a paragraph to be understood, redesign the control.
- Write copy as plain, short sentences: "You haven't allowed the Console to see
  users." not "Trellis could not reach the requested capability."
- Errors say what happened and what to do. Technical detail goes behind
  "Details".

## Choosing the presentation

Pick the form that fits the task. Tables are one tool, not the default.

- **Table**: many similar records that people scan, sort, and filter (users,
  services, sessions, events).
- **List with detail panel**: review queues and anything where one item is
  examined at a time (device activation review, job inspection).
- **Summary cards and small charts**: the Overview and other "is everything
  OK?" questions. Each metric shows its value, its trend, and links to where
  to act.
- **Grouped choices**: permission and access decisions. Group by what a person
  recognises (Users, Devices, Jobs), show counts, expand for detail.
- **Form**: creating or editing one thing. One column, labelled fields, the
  primary action at the bottom.

Keep one clear primary area per page. Avoid dashboards of equally weighted
boxes, and never show an empty frame where an empty state belongs.

## Layout

- **Sidebar** (light, same tone as the page): logo at the top; unlabeled
  primary group (Overview, Health, Events, Jobs, Sessions); then **Access**
  (Users, Roles, Connected apps, Apps) and **Platform** (Services, Devices,
  APIs, Resources). Account sits at the bottom. The active item is a filled
  primary pill.
- **Top bar**: search field, connection status pill, theme toggle, avatar menu.
- **Workspace**: a slightly lighter panel with a rounded top-left corner.
- **Page header**: optional small uppercase eyebrow, large title, one-line
  subtitle, primary action on the right ("+ New user").
- **Filters** sit in one row above the content: search on the left, dropdown
  filters and segmented tabs (All / Active / Disabled) next to it.

## Visual style

Warm, calm, and quiet. Colour communicates state and the primary action,
nothing else.

| Token        | Light     | Dark      | Use                              |
| ------------ | --------- | --------- | -------------------------------- |
| base-200     | `#f3f0e9` | `#1b1916` | page, sidebar                    |
| base-100     | `#fbfaf6` | `#24211d` | panels, inputs                   |
| base-300     | `#e3ddd1` | `#3a352e` | borders                          |
| base-content | `#2e2924` | `#ece6db` | text                             |
| primary      | `#8a7550` | `#b59a68` | primary buttons, active nav      |
| success      | `#4f7a3f` | `#8fb07a` | healthy, active                  |
| warning      | `#b8862a` | `#d9aa4c` | degraded, pending                |
| error        | `#a33d31` | `#d7705f` | failed, destructive              |
| info         | `#3f6283` | `#8fb0cf` | neutral highlights ("On demand") |

- Radii: panels `0.75rem`, fields and buttons `0.5rem`, badges `0.375rem`.
- Borders over shadows. No gradients, glass, or decorative imagery.
- Type: Inter or system sans. Page title 2rem semibold; section titles 1rem
  semibold; body 0.875–0.9375rem; muted text at ~65% opacity.
- Monospace only for values people copy: IDs, codes, subjects.
- The logo is the leaf mark (`art/trellis-mark.svg`) with the "Trellis"
  wordmark.

## Status

- Health is a coloured dot plus a word (● Healthy, ● Degraded, ● Offline).
- Lifecycle state (Active, Disabled, Archived, On demand, Pending) is a soft
  tinted badge.
- Never rely on colour alone. Keep administrative state, runtime health, and
  authority separate: an enabled service is not necessarily healthy.
- Preserve unknown values neutrally instead of mapping them to a reassuring
  default.

## Access and permissions

- People choose what an app may do. Ask at the level they understand: groups
  such as "Users" or "Jobs", with detail available on expansion.
- When something isn't allowed, show a calm "Not allowed yet" state with a
  **Change access** action — not an error banner.
- Hide navigation for areas the user cannot use at all.

## Data correctness

- A page of results is not a total; label counts with their scope ("Showing
  1–10").
- A missing or forbidden exact target shows an unavailable state with actions
  disabled; never silently select another record.
- Keep large integers, revisions, and timestamps exact when displayed; use the
  shared display helpers so `bigint` and byte values survive copy and search.

## Interaction

- Primary action: filled primary button. Secondary: outline. Tertiary: ghost
  or link.
- Destructive actions are red, named for what they do ("Disable device"), and
  confirm with the record's name.
- Row actions live in a trailing "⋯" menu.
- Subtle hover, visible focus rings, no heavy animation, keyboard operable.
