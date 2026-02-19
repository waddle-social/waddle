# Waddle GUI Deployment

`app/gui` is deployed to Cloudflare Workers as a static SPA build.

## Local Build (Bun-only)

```bash
bun install --frozen-lockfile
bun run build
```

## Deployment Behavior

- Pull requests:
  - Deploys a per-PR preview worker named `waddle-gui-pr-<PR_NUMBER>`.
  - Posts or updates a PR comment with the preview URL.
  - Deletes the preview worker when the PR is closed.
- Main branch:
  - Deploys production using `--env production`.
  - Production route is configured for `app.waddle.chat`.

## Domain Mapping

- `app.waddle.chat`: GUI Cloudflare Worker
- `waddle.chat`: marketing website
- `xmpp.waddle.chat`: XMPP server (GitOps-managed)
