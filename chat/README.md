# Chat

Astro + TypeScript starter for the `chat/` surface, scaffolded with the Astro CLI and wired for Cloudflare Workers, Vue, Tailwind 4, Bun, and `cuenv`.

## Commands

Run these from [`chat/`](/Users/icepuma/development/waddle/chat):

```sh
cuenv task install
cuenv task dev
cuenv task build
cuenv task deploy
```

If you need Cloudflare runtime types without a full build:

```sh
cuenv task generateTypes
```

## Notes

- `astro.config.ts` is the source of truth for Astro configuration.
- `wrangler.jsonc` is aligned with the Cloudflare adapter setup used in `colony/`.
- The `cuenv` tasks use Bun only. Do not introduce `npm`, `pnpm`, or `yarn`.
