# Chat

Minimal Astro + Svelte client for `waddle-server`, wired for Cloudflare Workers, Tailwind 4, Bun, and `cuenv`.

## Commands

Run these from [`chat/`](/Users/icepuma/development/waddle/chat):

```sh
cuenv task install
cuenv task dev
cuenv task build
cuenv task deployPreview
cuenv task deployProduction
```

If you need Cloudflare runtime types without a full build:

```sh
cuenv task generateTypes
```

## Local Setup

1. Copy `.dev.vars.example` to `.dev.vars`.
2. Set `SERVER_BASE_URL` in `.dev.vars` to `http://localhost:3000`.
3. Generate Cloudflare runtime types if needed:
   - `bun run generate-types`

## Notes

- `astro.config.mjs` is the source of truth for Astro configuration.
- `chat` does not authenticate against Colony directly. It sends the browser to `waddle-server`, which owns the Waddle session and delegates to Colony.
- `wrangler.jsonc` is aligned with the Cloudflare adapter setup used in `colony/`.
- `cuenv task dev` runs the Wrangler local runtime for the built worker.
- The `cuenv` tasks use Bun only. Do not introduce `npm`, `pnpm`, or `yarn`.

### Ambiguous legacy room IDs

The web client still uses local room IDs in selection and routes. If explicit
room discovery, selection or an explicit member-query override observes the
same ID at distinct bare room addresses, that ID is quarantined for the lifetime of the XMPP client object.
Refreshes, incomplete catalogs, reconnects and explicit selection cannot clear
it. Correct the catalog/bookmarks and reload to create a fresh client. Other
unique rooms remain usable; duplicate observations of the same address are
harmless under the existing bare-JID comparison convention.

Quarantined IDs cannot select a room, fall back to the inferred local service,
start ID-addressed history/admin operations, or enqueue new messages. An
inferred address before discovery is not itself authoritative evidence, so a
unique custom MUC service can still be discovered normally. Known addresses
remain bound across catalog omission; omission does not restore the local guess.
Previously started
operations and queued messages with an already captured room JID retain that
exact destination. This does not migrate routes or stored state, qualify space
identities, define new JID canonicalization, or enable federation.
