---
title: Capability Patterns
description: Capability approval, exact grants, and deployment policy.
---

# Capability Patterns

Capabilities are API-owned explanation and approval groups. Their identity is
`<api-id>::<name>`. Human consent fingerprints bind capability identity,
description, and consequence. Titles are presentation; exact `allows` membership
is machine semantics but not part of the consent fingerprint.

Approval does not grant a capability token. Trellis resolves selected actions,
public membership, approved current-consent capabilities, and the principal's
delegation ceiling into exact signed permission atoms. Unselected actions are
never granted. Overlapping capabilities combine by OR, while every implicated
nonoptional capability must still be approved for readiness.

Capability mode supports user consent and capability-based deployment approval.
Exact mode supports explicit administrative grants and does not auto-expand from
capability names. Platform Admin is separate and never inferred from application
capability text.

Optional capability decline leaves functionality unavailable without blocking
the participant. Provider definition compatibility is a readiness prerequisite;
socket liveness is reported separately and does not revoke an otherwise valid
login.

Resource approval is separate. Capabilities may explain actions, while resource
commitments authorize concrete State/KV/Store/Job/Consumer semantics. Desired
capacity is not approval.
