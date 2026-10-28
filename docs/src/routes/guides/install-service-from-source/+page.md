---
title: "Install a service from source"
description: "Install and run a service from its source tree during development."
---

# Install a service from source

Clone the project, install its declared language dependencies, then run
`trellis install` and `trellis generate`. The CLI reads native `.trellis` source
packages and frozen resolution metadata; it never executes TypeScript or Rust
authoring code.

For local API dependencies, use a `path` entry in `trellis.toml`. Registry dependencies remain exact and digest-verified through `trellis.lock`.

## Build a reproducible consumer

Use `trellis update` when intentionally changing dependency resolutions. For an
existing deployment checkout, use `trellis install` to reproduce its lock, then
`trellis generate` to build its private language SDKs. Run the project's own
format, check, test, and build commands before deployment. Do not edit generated
artifacts or rewrite imports as an installation step.

## Install and apply the participant

Log in to the target runtime with `trellis login <url>`. Accept the APIs the
service implements with
`trellis apis accept --source <project-root> --participant <package>.<Service>`,
then create the service deployment from verified source with
`trellis svc <name> create --source <project-root> --participant <package>.<Service>`,
then provision an instance with `trellis svc <deployment-id> provision`. Later
releases use `trellis svc <deployment-id> apply --source <project-root>`.
Create and apply compile and install the selected participant,
updates deployment resources, and writes its participant-scoped grant. See
[installing an image](/guides/install-service-from-image) for the matching
provisioning sequence.

## Run and update

Store the returned private identity seed securely and configure the service's
documented Trellis URL and identity seed. Start its normal run command.
Bootstrap resolves deployment/instance assignment, installed participant,
grants, transports, and resource handles; application code must not
inject NATS administrator credentials or binding payloads.

For an update, rebuild from the new IDL and source and restart the service.
It submits its own build: a build that asks for nothing new installs at once,
and one that asks for new capabilities or resources waits while it prints a
review link and QR code for an administrator (`trellis svc <deployment-id>
approve` does the same). You can still install first with `trellis apis accept`
and `trellis svc <deployment-id> apply`. Keep the same provisioned identity for
the same instance. Do not reprovision merely because the generated SDK or
application binary changed.
