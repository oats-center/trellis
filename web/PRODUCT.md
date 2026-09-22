# Trellis Console Product Context

## Register

product

## Product Purpose

Trellis Console is a distributed runtime control plane for operators managing
live Trellis systems. It helps users understand runtime state, inspect service
and device participation, review contracts, manage jobs and sessions, and make
operational decisions without shifting into raw protocol or log-level tooling.

The console is not an analytics dashboard, marketing surface, or exploratory
data product. It is a task-focused operator interface where clarity, density,
and consistent control patterns matter more than visual novelty.

## Primary Users

- Platform operators responsible for keeping Trellis deployments healthy.
- Service owners inspecting service instances, contracts, jobs, health events,
  and runtime permissions.
- Administrators managing users, sessions, grants, deployment authority,
  capability groups, portals, consolidated device records, activation reviews,
  and service reconciliation workflows.
- Developers diagnosing runtime integration issues during local or staging work.

## User Mindset

Users arrive with an operational question or a pending action. They need to scan
current state quickly, identify exceptions, and move to the right corrective
workflow with minimal interpretation. The interface should reduce uncertainty
during live-system work.

Common user questions include:

- What is connected, stale, degraded, or offline?
- Which service instance, device, portal, app, session, or contract needs
  attention?
- What capabilities, resources, NATS subjects, jobs, or health events explain
  this state?
- What action can I safely take next?

## Product Principles

- Prioritize operational truth over decoration.
- Keep the primary table or list visually dominant on each screen.
- Use secondary panels for summaries, warnings, health snapshots, and workflow
  context.
- Favor dense, structured layouts over open marketing-style space.
- Prefer explicit state labels, timestamps, identifiers, and capabilities over
  vague summary language.
- Make status, risk, and disabled/offline states visible without relying on
  color alone.
- Preserve Trellis platform boundaries: show derived runtime facts and
  contract-owned surfaces without implying unsupported authoring capabilities.

## Tone And Copy

Copy should be terse, specific, and operational. Labels should describe the
object or action directly. Empty states should explain what will appear there or
what action unlocks the view. Error messages should preserve useful context and
point toward the next operator action when possible.

Avoid sales language, broad reassurance, decorative slogans, and dashboard-style
narrative summaries. The console should sound like a reliable control plane, not
a product tour.

## Strategic Priorities

1. Rapid scanning of runtime state.
2. Accurate state awareness across services, devices, jobs, contracts, sessions,
   and grants.
3. Safe operational decision making.
4. Consistent navigation and control vocabulary.
5. Clear separation between primary operational data and secondary supporting
   context.

## Anti-References

Trellis Console should not resemble:

- Marketing SaaS landing pages.
- Analytics dashboards centered on charts and large metrics.
- Decorative data visualization products.
- Consumer admin panels with oversized cards and sparse content.
- Novel custom UI systems that replace standard controls for style.

## Design Relationship

`DESIGN.md` is the visual and structural contract for implementation. This file
defines product context and intent. When choosing between competing UI options,
prefer the one that is denser, clearer, more consistent with daisyUI primitives,
and more useful to an operator managing a live system.
