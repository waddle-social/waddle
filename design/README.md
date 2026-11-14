# @waddle/design

Polar Huddle, Waddle's shared design foundation: a Panda CSS 2 preset (tokens,
semantic tokens, text styles, keyframes, `wd-*` recipes, global CSS), the
self-hosted fonts and the brand illustrations.

## Use it in an app

Add the workspace and Panda (exact versions):

```jsonc
// package.json
"dependencies": { "@waddle/design": "workspace:*" },
"devDependencies": {
  "@pandacss/dev": "2.5.0",
  "@pandacss/preset-base": "2.5.0",
  "@pandacss/vite": "2.5.0"
}
```

```ts
// panda.config.ts
import { defineConfig } from '@pandacss/dev'
import { waddlePreset } from '@waddle/design'

export default defineConfig({
  presets: ['@pandacss/preset-base', waddlePreset],
  preflight: true,
  include: ['./src/**/*.{ts,tsx,astro}'],
  outdir: 'styled-system',
  // jsxFramework: 'react', // only for apps that use JSX style props
})
```

Astro (7) and other Vite apps use `@pandacss/vite`, which runs codegen and CSS
generation inline:

```ts
// astro.config.ts
import panda from '@pandacss/vite'
export default defineConfig({ vite: { plugins: [panda()] } })
```

```css
/* src/styles/global.css: the file declaring the layers is where Panda injects its CSS */
@layer reset, base, tokens, recipes, utilities;
```

```astro
---
import '@waddle/design/fonts.css'
import '../styles/global.css'
import { btn } from '../../styled-system/recipes'
---
<button class={btn({ variant: 'primary' })}>Join huddle</button>
```

Assets: `import mark from '@waddle/design/assets/waddle-mark.svg?url'` (or copy
them from `design/assets/`). Fonts come only from `@fontsource-variable/*`;
never load Google Fonts.

## Theming contract

- Put `data-theme="night"` or `data-theme="daylight"` on `<html>`. Night is the
  default when the attribute is absent. Any element can carry `data-theme` to
  switch its subtree; every theme-dependent token is re-declared on it.
- Conditions: `_night`/`_dark` and `_daylight`/`_light`.
- Density: `data-density="compact|roomy"` (comfortable default) changes
  `sizes.rowH`, `chipH`, `controlH`, `msgAvatar` and `spacing.msgPadY`. Coarse
  pointers always get 44px rows, chips and controls (`_coarse`).
- Reduced motion collapses the `--wd-rise`, `--wd-press`, … distances that
  every keyframe reads, so entrances become fades and loops stop.

## Recipes

Recipes keep the `wd-*` class names (`wd-btn`, `wd-btn--variant_primary`).
Parts keep their bundle class names and are styled by the block recipe, so add
them as plain classes: `wd-msg__meta`, `wd-msg__author`, `wd-msg__time`,
`wd-msg__body`, `wd-msg__actions`, `wd-room__name`, `wd-chip__n`,
`wd-bubble__time`, `wd-card__mark`, `wd-dialog__head`, `wd-dialog__foot`.
States stay on ARIA/data attributes: `aria-pressed`, `aria-selected`,
`aria-current="page"`, `aria-checked`, `aria-invalid`, `data-unread`,
`data-speaking`, `data-fresh`, `data-stuck`, `data-closing`, `data-joining`,
`data-off`.

`bun test` builds a fixture with the preset and checks the generated CSS.
