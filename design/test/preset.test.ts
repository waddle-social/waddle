import { afterAll, expect, test } from 'bun:test'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

// Inside the package so the fixture resolves @pandacss/* from design/node_modules.
const dir = mkdtempSync(join(import.meta.dir, '.fixture-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

const panda = (...args: string[]) => {
  const run = Bun.spawnSync([join(import.meta.dir, '../node_modules/.bin/panda'), ...args, '--cwd', dir, '--no-color'])
  const out = run.stdout.toString() + run.stderr.toString()
  expect(run.exitCode, out).toBe(0)
  return out
}

test('preset generates themed tokens and every recipe', async () => {
  writeFileSync(
    join(dir, 'panda.config.ts'),
    `import { defineConfig } from '@pandacss/dev'
import { waddlePreset } from '../../src/preset'
export default defineConfig({
  presets: ['@pandacss/preset-base', waddlePreset],
  preflight: true,
  include: ['./app.ts'],
  outdir: 'styled-system',
  staticCss: { recipes: '*' },
})
`,
  )
  writeFileSync(join(dir, 'app.ts'), `import { css } from './styled-system/css'\ncss({ bg: 'surface', textStyle: 'hero', _daylight: { color: 'fg.muted' } })\n`)

  panda('codegen')
  expect(panda('cssgen', '-o', 'out.css')).toContain('diagnostics: 0')
  // Generated recipe modules must load (catches names that are not valid identifiers).
  const recipes = await import(join(dir, 'styled-system/recipes/index.js'))
  expect(recipes.btn({ variant: 'primary', size: 'sm' })).toBe('wd-btn wd-btn--variant_primary wd-btn--size_sm')
  const out = readFileSync(join(dir, 'out.css'), 'utf8')

  expect(out).toMatch(/\[data-theme=daylight\] \{[^}]*--colors-bg: var\(--colors-ice-50\)/)
  expect(out).toContain('.wd-btn--variant_primary')
  expect(out).toContain('.wd-avatar--tint_6')
  expect(out).toContain('@keyframes wd-waddle')
  expect(out).toContain('--fonts-display: var(--font-display)')
  // Every {category.path} reference resolved to a var().
  expect(out).not.toMatch(/\{(colors|spacing|sizes|radii|durations|easings|fonts|fontSizes|shadows|gradients|borderWidths)\.[^}]*\}/)
})
