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

## Deployment Commands (cuenv)

```bash
# Preview
PR_NUMBER=123 cuenv exec --path . --env production \
  bun x wrangler deploy --config wrangler.jsonc --name waddle-gui-pr-$PR_NUMBER

# Production
cuenv exec --path . --env production \
  bun x wrangler deploy --config wrangler.jsonc --env production
```

## Domain Mapping

- `app.waddle.chat`: GUI Cloudflare Worker
- `waddle.chat`: marketing website
- `xmpp.waddle.chat`: XMPP server (GitOps-managed)
