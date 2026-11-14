// Polar Huddle families for an app's astro.config `fonts:` key (Astro Fonts API).
// Panda's fonts.* tokens read these CSS variables.
import type { AstroUserConfig } from 'astro'
import { fontProviders } from 'astro/config'

const sans = ['system-ui', '-apple-system', 'Segoe UI', 'Roboto', 'Noto Sans', 'sans-serif']

export const waddleFonts = [
  {
    provider: fontProviders.fontsource(),
    name: 'Bricolage Grotesque',
    cssVariable: '--font-display',
    weights: ['500 800'],
    styles: ['normal'],
    subsets: ['latin', 'latin-ext'],
    fallbacks: ['Atkinson Hyperlegible Next', 'system-ui', '-apple-system', 'Segoe UI', 'sans-serif'],
  },
  {
    provider: fontProviders.fontsource(),
    name: 'Atkinson Hyperlegible Next',
    cssVariable: '--font-body',
    weights: ['400 800'],
    styles: ['normal', 'italic'],
    subsets: ['latin', 'latin-ext'],
    fallbacks: sans,
  },
  {
    provider: fontProviders.fontsource(),
    name: 'Atkinson Hyperlegible Mono',
    cssVariable: '--font-mono',
    weights: ['400 700'],
    styles: ['normal', 'italic'],
    subsets: ['latin', 'latin-ext'],
    fallbacks: ['ui-monospace', 'SF Mono', 'Menlo', 'Consolas', 'monospace'],
  },
] satisfies NonNullable<AstroUserConfig['fonts']>
