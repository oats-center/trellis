# Trellis Console — DESIGN.md (Stitch-Optimized)

## PURPOSE

This file defines the visual, structural, and behavioral rules for generating
Trellis Console UI.

Trellis Console is a **distributed runtime control plane**.

All generated UI MUST follow this specification.

---

## PRIMARY DESIGN INTENT

Trellis Console is built for **operators managing live systems**, not for
analytics or exploration.

The UI MUST optimize for:

1. Rapid scanning
2. State awareness
3. Operational decision making

The UI MUST NOT resemble:

- analytics dashboards
- marketing SaaS tools
- data visualization products

---

## HARD CONSTRAINTS (DO NOT VIOLATE)

- DO NOT use charts as primary UI
- DO NOT use a card as the primary or only thing on any screen
- DO NOT use DaisyUI `card` as default panel chrome
- DO NOT create large card grids
- DO NOT exceed 4 primary panels on screen
- DO NOT introduce new colors outside defined tokens
- DO NOT use decorative UI elements
- DO NOT use excessive whitespace

`card` is permitted only where it is a very good fit to the data being presented
(rare). The default content container is the bordered section primitive
(`Panel.svelte` / `trellis-section`), never a DaisyUI card frame.

Charts are permitted as **secondary** diagnostic artifacts only when they
explain a specific data series that the operator is already looking at. The
table or list must remain the dominant visual element on every screen.
Whole-system dashboards, large metric tiles, and chart-only views are not
allowed.

---

## LAYOUT SYSTEM

### Global Layout

ALWAYS use:

- Left: persistent navigation (drawer)
- Top: command bar (search, connection state, user)
- Main: structured panels

---

### Overview Page Layout (REQUIRED STRUCTURE)

Order MUST be:

1. Runtime Topology (horizontal strip)
2. Metrics Strip (inline, NOT cards)
3. Main Grid:
   - Left (primary): Service Instances table
   - Right (secondary): stacked panels
4. Bottom: Recent Activity

---

### Panel Hierarchy

Panels are classified as:

- Primary: tables (dominant visual weight)
- Secondary: summaries (health, jobs, warnings)

Rules:

- Only ONE primary panel per screen
- Secondary panels MUST be stacked, not grid-distributed
- Avoid equal visual weight across panels

---

## COMPONENT SYSTEM (DAISYUI RULES)

### REQUIRED COMPONENTS

Use DaisyUI primitives and the Trellis section layer:

- Layout: `drawer`, `navbar`, `Panel` (`trellis-section` bordered sections)
- Controls: `btn`, `badge`, `input`
- Data: `table` (`DataTable`), `progress`, `MetricsLedger`, `StatusBadge`

---

### ALLOWED CUSTOMIZATION

Only override DaisyUI when:

- Density is too low
- Spacing is too loose
- Visual weight is too high

Allowed overrides:

- Tighten table row height
- Adjust spacing

---

### FORBIDDEN CHANGES

- Do not replace Daisy primitives
- Do not introduce custom component systems
- Do not create alternative button or badge styles

---

## DENSITY RULES (CRITICAL)

### Tables

- Row height: 44–52px
- Identifiers MUST NOT wrap
- Use monospace for identifiers
- Fixed column layout

### Panels

- No card frame, no shadow
- Thin border
- Header height: ~56px

### Spacing

- Tight vertical rhythm
- Consistent horizontal padding
- No large empty areas

---

## STATUS SYSTEM (STRICT)

### Status Mapping

| Status    | Daisy Class   |
| --------- | ------------- |
| Healthy   | badge-success |
| Degraded  | badge-warning |
| Unhealthy | badge-error   |
| Offline   | badge-neutral |

---

### Rules

- ALWAYS use badges for status
- NEVER invent new colors
- Use moderate saturation (not bright/neon)

---

## DATA PRESENTATION RULES

### Identifiers

MUST use monospace for:

- instance IDs
- contract names
- subjects / RPC paths

---

### State, Health, and Authority

Render administrative state, runtime health, and authorization as separate
concepts. An active deployment or identity does not establish that a process is
healthy or connected, and a heartbeat does not establish authority. Preserve
unknown wire values neutrally instead of mapping them to a reassuring default.

### Partial Reads and Exact Targets

A page of results is not a global total; label counts with their scope. Editors
must preserve existing values that a discovery catalog does not contain, and a
missing or forbidden exact target renders an unavailable state with mutations
disabled rather than silently selecting another record.

### Displaying Generated Values

Large integers, byte fields, and timestamps are generated wire values. Keep
revisions, sequence numbers, and IDs exact for display and comparison; render
byte fields explicitly rather than as comma-separated numbers. Use the shared
display helper for JSON previews, copy, and search so `bigint` and byte values
survive a round trip.

---

### Tables

- Left aligned text
- Compact headers
- Uppercase headers
- Small font size for headers

---

### Metadata

- Secondary text only
- Muted color
- Smaller font size

---

## SCREEN DEFINITIONS

### Overview (REFERENCE IMPLEMENTATION)

MUST include:

- Service Instances table (primary)
- Live Health panel
- Jobs snapshot
- Contract warnings
- Recent activity

---

### Health Events

Structure:

- Participants table (primary)
- Heartbeat stream (secondary)

---

### Contracts

Structure:

- Split pane layout (REQUIRED)
  - Left: contract list
  - Right: contract detail

Detail MUST include:

- RPC methods
- Events
- KV resources
- Store and jobs resources
- Derived NATS publish/subscribe rules
- Required capabilities

Contracts UI MUST NOT imply v1 supports caller-authored raw subject declarations
or arbitrary stream resource requests. Runtime NATS subjects may be displayed as
derived permissions only.

---

### Jobs

Structure:

- Filter bar (search, state pills, service, window, more filters)
- Inline metrics strip (total, queued, running, failed, workers)
- Secondary job-type matrix: one row per job type with backlog, running, failed,
  DLQ, queue wait p95, runtime p95, and a small sparkline for recent failures
- Optional scoped diagnostic charts (event throughput and latency over time) for
  the selected job type
- Table only (primary): the workbench jobs table
- Side panel (secondary): job inspector

MUST include:

- job state badges
- filtering by service
- click-to-scope behavior: selecting a job type filters the table and reveals
  the scoped diagnostic charts for that type

---

### Device Reviews

Structure:

- Review list (primary)
- Side panel (secondary)

MUST include:

- approve / reject actions
- copy that explains review decisions complete the original activation operation
- metadata visibility toggle

---

## VISUAL STYLE

### Base Colors (Daisy Tokens)

- Background: `--b2`
- Panels: `--b1`
- Borders: `--b3`
- Text: `--bc`

---

### Status Colors

- Success: `--su`
- Warning: `--wa`
- Error: `--er`
- Info: `--in`

---

### Effects

- Minimal shadows
- Prefer borders over elevation
- Subtle lattice/mesh motifs allowed ONLY in background areas

---

## INTERACTION RULES

- Hover states MUST be subtle
- Table rows highlight on hover
- Buttons use `ghost` or `outline`
- No heavy animation

---

## ANTI-PATTERNS (STRICTLY FORBIDDEN)

- Charts or graphs as primary UI
- Cards as primary content or default panel chrome
- Large card grids
- Uneven spacing
- Decorative UI elements
- Marketing-style layouts

---

## GENERATION GUIDELINES (FOR AI SYSTEMS)

When generating UI:

1. Start from layout structure
2. Place primary table first
3. Add only necessary secondary panels
4. Apply density rules
5. Apply status system
6. Validate against anti-patterns

If uncertain:

> Prefer simpler, denser, more structured UI

---

## SUMMARY

Trellis Console is:

> A structured, dense, operator-first control plane UI

All generated UI MUST reinforce:

- clarity
- hierarchy
- consistency
- operational efficiency
