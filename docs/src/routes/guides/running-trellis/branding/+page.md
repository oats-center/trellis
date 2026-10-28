---
title: "Brand the sign-in page"
description: "Give the sign-in and consent page your name, logo, colours and text, for the whole installation or one app."
---

The sign-in and consent page can carry your name, logo and colours, for the
whole installation and for individual apps. Edit it in the Console under
**Platform → Sign-in page**, or with the CLI:

```sh
trellis theme set \
  --name "Acme Cloud" \
  --logo ./branding/logo.svg \
  --stylesheet ./branding/portal.css \
  --color primary=#0050b3 --color primary-hover=#003f8f \
  --dark-color primary=#4d8fe0 \
  --copy "sign_in_title=Sign in to Acme" \
  --copy "footer=Need help? support@acme.example"

# Overrides for one app, layered over the installation theme:
trellis theme set --client trellis.console --name "Acme Console" \
  --logo ./branding/console.svg

trellis theme show --client trellis.console
trellis theme remove --client trellis.console
```

`set` replaces the whole theme, so anything you leave out is cleared. Logos can
be PNG, JPEG, WebP or SVG up to 256 KiB, and the stylesheet (installation theme
only) up to 64 KiB; it loads after the page's own. Colours take hex values for
`page`, `surface`, `ink`, `muted`, `line`, `line-strong`, `primary`,
`primary-hover`, `on-primary` and `soft`. The text you can replace is
`sign_in_title`, `sign_in_subtitle` and `footer`.

A theme changes how the page looks, never what it does. Sign-in stays on its
own address, consent shows exactly what Trellis asks it to, and apps never see
a token. The page picks an app's theme from its own record of the sign-in in
progress rather than from the link, so a crafted link can't dress one app up as
another, and theme files are served so that even an SVG can't run script.

Changing the installation theme takes permission to manage the installation;
an app's theme takes permission to manage apps. Changes apply immediately.
