# Administrative Capabilities

Status: implemented.

Trellis has one permission system: capabilities. Managing the Trellis
installation itself is done with six **administrative capabilities**.

## Names

The administrative capabilities belong to the built-in Auth API:

| Capability                          | Allows                                                    |
| ----------------------------------- | --------------------------------------------------------- |
| `trellis.auth.v1:users.manage`      | users, services, devices, resources, sign-in providers    |
| `trellis.auth.v1:roles.manage`      | roles, assignments, direct grants, provider role mappings |
| `trellis.auth.v1:apis.accept`       | reviewing and accepting compatible API updates            |
| `trellis.auth.v1:apis.forceReplace` | installing API updates that break compatibility           |
| `trellis.auth.v1:clients.manage`    | registering apps and changing their sign-in settings      |
| `trellis.auth.v1:platform.manage`   | who administers, issuer keys                              |

The built-in Auth contract declares them with the IDL `administrative;` marker,
which only the built-in `trellis` package may use. Auth refuses to review or
accept any API in the `trellis.` namespace other than the ones Trellis ships.
Every capability is named `area.action` (`sessions.read`, `account.changePassword`,
`users.manage`).

They are ordinary capabilities otherwise: roles, direct grants, and
identity-provider mappings can all carry them. Mapping administration to a role
or a provider group is an administrator's decision. Changing who holds an
administrative capability, by any of those paths, requires
`trellis.auth.v1:platform.manage`.

## The default administrator

The administrator created at initialization holds every administrative
capability inherently, not through a grant, so no grant, role, or mapping change
can take them away, and the account cannot be disabled. There is therefore
always someone who can approve administrative delegations.

## Delegation safeguards

Delegating administration to an app has two safeguards. Both are on by
default and configurable:

```toml
[auth.administration]
confirm_on_consent = true
registered_clients_only = true
```

- **Confirm on consent.** After selection, a separate step states that the app
  is asking to manage this Trellis installation itself and lists what it may
  do; approving it confirms. Select all never selects administration, and
  administrative choices are never remembered. An administrator's confirmed
  delegation reaches the app at once: administrators administer, and nobody
  else approves their choice.
- **Registered clients only.** Only OAuth clients an administrator registered
  may receive administrative capabilities; a client known only from its
  metadata document cannot.
