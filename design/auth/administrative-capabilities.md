# Administrative Capabilities

Status: implemented.

Trellis has one permission system: capabilities. Managing the Trellis
installation itself is done with six **administrative capabilities**.

## Names

Administrative capabilities live in a reserved namespace, `trellis.*`:

| Capability                 | Allows                                                    |
| -------------------------- | --------------------------------------------------------- |
| `trellis.managePrincipals` | users, services, devices, resources, sign-in providers    |
| `trellis.manageRoles`      | roles, assignments, direct grants, provider role mappings |
| `trellis.acceptApis`       | reviewing and accepting compatible API updates            |
| `trellis.forceReplaceApis` | installing API updates that break compatibility           |
| `trellis.manageClients`    | registering apps and changing their sign-in settings      |
| `trellis.managePlatform`   | who administers, issuer keys, administration reviews      |

The built-in Auth contract declares them with the IDL `administrative;` marker,
which only the built-in `trellis` package may use. Auth refuses to review or
accept any API in the `trellis.` namespace other than the ones Trellis ships.
Every capability name is verb first (`readSessions`, `changePassword`,
`managePrincipals`).

They are ordinary capabilities otherwise: roles, direct grants, and
identity-provider mappings can all carry them. Mapping administration to a role
or a provider group is an administrator's decision. Changing who holds an
administrative capability, by any of those paths, requires
`trellis.managePlatform`.

## The default administrator

The administrator created at initialization holds every administrative
capability inherently, not through a grant, so no grant, role, or mapping change
can take them away, and the account cannot be disabled. There is therefore
always someone who can approve administrative delegations.

## Delegation safeguards

Delegating administration to an app has three safeguards. All are on by
default and configurable:

```toml
[auth.administration]
confirm_on_consent = true
require_peer_approval = true
registered_clients_only = true
```

- **Confirm on consent.** After selection, a separate step states that the app
  is asking to manage this Trellis installation itself, which is rare and
  exceptional, and requires explicit confirmation. Select all never selects
  administration, and administrative choices are never remembered.
- **Peer approval.** The grant is created without its administrative
  capabilities and a review opens per user, app binding, and capability.
  Someone holding `trellis.managePlatform` other than the requester decides it
  with `Administration.Reviews.Decide`. Approval reaches the user's live grants
  for that binding and later grants reuse it; rejection withholds it until the
  user asks again. The default administrator's own delegations need no review.
- **Registered clients only.** Only OAuth clients an administrator registered
  may receive administrative capabilities; a client known only from its
  metadata document cannot.
