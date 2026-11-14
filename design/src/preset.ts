// Waddle · Polar Huddle, as a Panda CSS 2 preset. Ported from the design
// reference ds/tokens.css and ds/components/bundle.css. Night is the base
// theme; `data-theme="daylight"` on any element switches its subtree.
import { definePreset } from '@pandacss/dev'
import type { SystemStyleObject } from '@pandacss/types'

const steps = [50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950, 975]
const ramp = (hue: number, lc: string) =>
  Object.fromEntries(lc.split(', ').map((v, i) => [steps[i], { value: `oklch(${v} ${hue})` }]))

// Theme-dependent value. Declared for the root, every [data-theme="night"] and
// every [data-theme="daylight"] so a nested theme recomputes values that
// reference other semantic tokens (state layers, focus ring, material).
const t = (night: string, day = night) => ({ value: { base: night, _night: night, _daylight: day } })

const NIGHT = ':where(:root:not([data-theme=daylight]), [data-theme=night]) &'
const DAYLIGHT = '[data-theme=daylight] &'

// Motion distances: plain custom properties so keyframes can read them and
// reduced motion can collapse them in one place.
const motion = {
  '--wd-rise': '8px',
  '--wd-slide': '16px',
  '--wd-pop-from': '0.6',
  '--wd-settle-from': '0.97',
  '--wd-grow-from': '0',
  '--wd-press': '0.97',
  '--wd-lift': '-1px',
  '--wd-nudge': '1px',
}
const motionReduced = {
  '--wd-rise': '0px',
  '--wd-slide': '0px',
  '--wd-pop-from': '1',
  '--wd-settle-from': '1',
  '--wd-grow-from': '1',
  '--wd-press': '1',
  '--wd-lift': '0px',
  '--wd-nudge': '0px',
}

const font = (family: string, weight: number, size: string, lineHeight: string, letterSpacing?: string) => ({
  fontFamily: family,
  fontWeight: weight,
  fontSize: size,
  lineHeight,
  ...(letterSpacing ? { letterSpacing } : {}),
})

const transition =
  'background-color {durations.fast} {easings.out}, border-color {durations.fast} {easings.out}, color {durations.fast} {easings.out}, box-shadow {durations.base} {easings.out}, scale {durations.base} {easings.spring.snappy}, translate {durations.base} {easings.out}'
const layer = (state: 'hover' | 'press') => `linear-gradient({colors.state.${state}}, {colors.state.${state}})`
const sheen = '{gradients.sheen}'
const raised = 'inset 0 1px 0 rgba(255, 255, 255, 0.2), 0 1px 2px rgba(0, 0, 0, 0.18)'
const press = { scale: 'var(--wd-press)', transitionDuration: 'instant' }
const lift = { '@media (hover: hover)': { 'a&:hover': { translate: '0 var(--wd-lift)', boxShadow: 'elev.2' } } }
const forcedBorder = { '@media (forced-colors: active)': { border: '1px solid CanvasText' } }
const glass = 'blur(20px) saturate(160%)'
const tail = '{radii.bubble} {radii.bubble} {radii.bubble} {radii.tag}'
const concentric = 'max({radii.xs}, calc(var(--r-out) - var(--pad)))'
const focusHalo = '0 0 0 6px color-mix(in srgb, {colors.focus} 18%, transparent)'
const icon = (size: string) => ({ width: size, height: size })

const btnDisabled = {
  _disabled: { bg: 'surface.2', borderColor: 'border', color: 'fg.soft', cursor: 'not-allowed', bgImage: 'none', scale: 'none' },
}
const btnTone = (tone: 'action' | 'live' | 'danger'): SystemStyleObject => ({
  bg: tone,
  borderColor: tone,
  color: `${tone}.fg`,
  fontWeight: 700,
  bgImage: sheen,
  boxShadow: raised,
  _hover: { bg: `${tone}.hover`, borderColor: `${tone}.hover`, bgImage: `${sheen}, ${layer('hover')}` },
  ...btnDisabled,
})

export const waddlePreset = definePreset({
  name: '@waddle/design',

  conditions: {
    extend: {
      night: NIGHT,
      daylight: DAYLIGHT,
      dark: NIGHT,
      light: DAYLIGHT,
      compact: '[data-density=compact] &',
      roomy: '[data-density=roomy] &',
      coarse: '@media (pointer: coarse)',
      narrow: '@media (max-width: 640px)',
    },
  },

  globalCss: {
    ':root': { colorScheme: 'dark', ...motion },
    '[data-theme=night]': { colorScheme: 'dark' },
    '[data-theme=daylight]': { colorScheme: 'light' },
    '@media (prefers-reduced-motion: reduce)': {
      ':root': motionReduced,
      '*, *::before, *::after': { animationIterationCount: '1 !important', scrollBehavior: 'auto !important' },
    },
    body: {
      margin: 0,
      bg: 'bg',
      color: 'fg',
      font: '400 {fontSizes.md}/1.5 {fonts.body}',
      fontOpticalSizing: 'auto',
      fontKerning: 'normal',
      fontSynthesis: 'none',
      WebkitFontSmoothing: 'antialiased',
      MozOsxFontSmoothing: 'grayscale',
    },
    ':where(h1, h2, h3, h4)': { fontFamily: 'display', textWrap: 'balance' },
    ':where(p, li, dd, blockquote)': { textWrap: 'pretty' },
    ':where(code, kbd, samp, pre)': { fontFamily: 'mono', fontSize: '0.9em' },
    ':where(time, td, .numeric)': { fontVariantNumeric: 'tabular-nums' },
    ':where(input, textarea, select, button)': { font: 'inherit' },
    '::selection': { bg: 'selection', color: 'inherit' },
    ':focus-visible': {
      outline: '{borderWidths.focus} solid {colors.focus}',
      outlineOffset: '2px',
      transition: 'outline-offset {durations.fast} {easings.out}',
      '@media (forced-colors: active)': { outlineColor: 'Highlight' },
    },
    'a:not([class])': {
      textUnderlineOffset: '3px',
      textDecorationThickness: '1px',
      transition: 'text-decoration-thickness {durations.fast} {easings.out}, color {durations.fast} {easings.out}',
      _hover: { textDecorationThickness: '2px' },
    },
    '*': { scrollbarWidth: 'thin', scrollbarColor: '{colors.border} transparent' },
    'svg[stroke-width="1.75"] :is(path, circle, rect, line, polyline, polygon, ellipse)': { vectorEffect: 'non-scaling-stroke' },
    '[role="tab"], [role="option"], [role="menuitem"], article': { transition },
    '[role="tab"]:active, [role="option"]:active': press,
    article: { boxShadow: 'elev.1' },
    '@media (hover: hover)': {
      'article:not([role="dialog"] article, [role="log"] article):hover': { translate: '0 var(--wd-lift)', boxShadow: 'elev.2' },
    },
    'nav, aside': { '--ring-bg': '{colors.bg.rail}' },
    '[role="dialog"]': {
      '--ring-bg': '{colors.surface}',
      animation: 'wd-dialog {durations.slow} {easings.spring.snappy} backwards',
      '&[data-closing]': { animation: 'wd-out {durations.base} {easings.in} forwards' },
    },
    '[role="log"] > *': { animation: 'wd-rise {durations.base} {easings.out} backwards' },
    '[role="log"] > *:nth-last-child(1)': { animationDelay: '80ms' },
    '[role="log"] > *:nth-last-child(2)': { animationDelay: '40ms' },
  },

  theme: {
    extend: {
      tokens: {
        colors: {
          ice: ramp(252, '0.985 0.004, 0.955 0.006, 0.905 0.009, 0.830 0.013, 0.710 0.018, 0.585 0.020, 0.470 0.022, 0.375 0.024, 0.290 0.026, 0.235 0.026, 0.200 0.026, 0.165 0.025'),
          floe: ramp(252, '0.975 0.012, 0.945 0.027, 0.895 0.053, 0.830 0.087, 0.760 0.117, 0.680 0.130, 0.580 0.130, 0.480 0.120, 0.380 0.101, 0.290 0.078, 0.215 0.055, 0.165 0.039'),
          beak: ramp(82, '0.980 0.019, 0.955 0.043, 0.920 0.078, 0.885 0.115, 0.845 0.158, 0.760 0.157, 0.660 0.136, 0.550 0.113, 0.440 0.091, 0.330 0.068, 0.240 0.050, 0.180 0.035'),
          aurora: ramp(200, '0.975 0.016, 0.945 0.033, 0.910 0.059, 0.865 0.091, 0.810 0.117, 0.720 0.122, 0.600 0.102, 0.500 0.085, 0.400 0.068, 0.300 0.051, 0.220 0.038, 0.170 0.030'),
          moss: ramp(152, '0.975 0.018, 0.945 0.037, 0.895 0.068, 0.830 0.105, 0.760 0.135, 0.680 0.150, 0.580 0.150, 0.480 0.126, 0.380 0.100, 0.290 0.077, 0.215 0.057, 0.165 0.044'),
          ember: ramp(52, '0.975 0.014, 0.945 0.031, 0.895 0.062, 0.830 0.107, 0.760 0.153, 0.680 0.170, 0.580 0.149, 0.480 0.124, 0.380 0.098, 0.290 0.075, 0.215 0.056, 0.165 0.044'),
          krill: ramp(22, '0.975 0.012, 0.945 0.027, 0.895 0.055, 0.830 0.094, 0.760 0.143, 0.680 0.190, 0.580 0.190, 0.480 0.175, 0.380 0.148, 0.290 0.114, 0.215 0.080, 0.165 0.057'),
        },
        // 4pt grid. Half steps are for inside controls only.
        spacing: {
          '0.5': { value: '2px' },
          1: { value: '4px' },
          '1.5': { value: '6px' },
          2: { value: '8px' },
          '2.5': { value: '10px' },
          3: { value: '12px' },
          4: { value: '16px' },
          5: { value: '20px' },
          6: { value: '24px' },
          8: { value: '32px' },
          10: { value: '40px' },
          12: { value: '48px' },
          16: { value: '64px' },
        },
        // Concentric: inner = max(xs, outer - padding).
        radii: {
          xs: { value: '4px' },
          sm: { value: '8px' },
          tag: { value: '6px' },
          control: { value: '12px' },
          panel: { value: '16px' },
          card: { value: '20px' },
          bubble: { value: '20px' },
          sheet: { value: '28px' },
          space: { value: '30%' },
          pill: { value: '999px' },
        },
        borderWidths: {
          hair: { value: '1px' },
          strong: { value: '2px' },
          focus: { value: '3px' },
        },
        zIndex: {
          base: { value: 0 },
          sticky: { value: 10 },
          panel: { value: 20 },
          dropdown: { value: 30 },
          scrim: { value: 40 },
          dialog: { value: 50 },
          toast: { value: 60 },
          tooltip: { value: 70 },
        },
        sizes: {
          sidebar: { DEFAULT: { value: '280px' }, min: { value: '240px' }, rail: { value: '72px' } },
          panel: { DEFAULT: { value: '360px' }, min: { value: '320px' } },
          header: { value: '56px' },
          page: { value: '1000px' },
          form: { value: '760px' },
          read: { value: '720px' },
          dialog: { sm: { value: '440px' }, md: { value: '520px' }, lg: { value: '640px' } },
          mobileHeader: { value: '56px' },
          mobileTabbar: { value: '64px' },
          target: { DEFAULT: { value: '44px' }, min: { value: '36px' } },
          measure: { body: { value: '68ch' }, message: { value: '72ch' }, lead: { value: '60ch' } },
        },
        durations: {
          instant: { value: '90ms' },
          fast: { value: '150ms' },
          base: { value: '220ms' },
          slow: { value: '320ms' },
          slower: { value: '480ms' },
          ambient: { value: '2400ms' },
        },
        easings: {
          out: { value: 'cubic-bezier(0.2, 0.7, 0.2, 1)' },
          in: { value: 'cubic-bezier(0.4, 0, 1, 1)' },
          inOut: { value: 'cubic-bezier(0.65, 0, 0.35, 1)' },
          spring: {
            DEFAULT: { value: 'cubic-bezier(0.34, 1.56, 0.64, 1)' },
            snappy: {
              value:
                'linear(0, 0.04, 0.135, 0.258, 0.389, 0.516, 0.63, 0.728, 0.809, 0.873, 0.923, 0.959, 0.984, 1.001, 1.012, 1.018, 1.02, 1.02, 1.018, 1.016, 1.013, 1.011, 1.008, 1.006, 1.004, 1)',
            },
            bouncy: {
              value:
                'linear(0, 0.062, 0.213, 0.404, 0.6, 0.775, 0.917, 1.02, 1.085, 1.118, 1.126, 1.117, 1.097, 1.072, 1.048, 1.026, 1.009, 0.996, 0.989, 0.985, 0.984, 0.985, 0.988, 0.991, 0.994, 1)',
            },
          },
        },
        // CSS variables set by the Astro Fonts API (see astro-fonts.ts).
        fonts: {
          display: { value: 'var(--font-display)' },
          body: { value: 'var(--font-body)' },
          mono: { value: 'var(--font-mono)' },
        },
        // Variable fonts: any value 400–800 is valid.
        fontWeights: {
          regular: { value: 400 },
          medium: { value: 500 },
          semibold: { value: 600 },
          bold: { value: 700 },
          heavy: { value: 800 },
        },
        fontSizes: {
          '2xs': { value: '12px' },
          xs: { value: '13px' },
          sm: { value: '14px' },
          md: { value: '15px' },
          base: { value: '16px' },
          lg: { value: '18px' },
          xl: { value: '20px' },
          '2xl': { value: '24px' },
          '3xl': { value: '30px' },
          '4xl': { value: '40px' },
          '5xl': { value: '60px' },
          // Marketing fluid scale, 360→1440px viewport, 16→18 base, 1.2→1.25 ratio.
          'step--1': { value: 'clamp(0.8333rem, 0.8111rem + 0.0988vw, 0.9000rem)' },
          'step-0': { value: 'clamp(1.0000rem, 0.9583rem + 0.1852vw, 1.1250rem)' },
          'step-1': { value: 'clamp(1.2000rem, 1.1312rem + 0.3056vw, 1.4062rem)' },
          'step-2': { value: 'clamp(1.4400rem, 1.3341rem + 0.4708vw, 1.7578rem)' },
          'step-3': { value: 'clamp(1.7280rem, 1.5716rem + 0.6952vw, 2.1973rem)' },
          'step-4': { value: 'clamp(2.0736rem, 1.8493rem + 0.9970vw, 2.7466rem)' },
          'step-5': { value: 'clamp(2.4883rem, 2.1734rem + 1.3999vw, 3.4332rem)' },
          'step-6': { value: 'clamp(2.9860rem, 2.5508rem + 1.9341vw, 4.2915rem)' },
          'step-7': { value: 'clamp(3.5832rem, 2.9894rem + 2.6389vw, 5.3644rem)' },
        },
        lineHeights: {
          '2xs': { value: '16px' },
          xs: { value: '18px' },
          sm: { value: '20px' },
          md: { value: '22px' },
          base: { value: '24px' },
          lg: { value: '24px' },
          xl: { value: '26px' },
          '2xl': { value: '30px' },
          '3xl': { value: '36px' },
          '4xl': { value: '44px' },
          '5xl': { value: '60px' },
        },
        letterSpacings: {
          '2xs': { value: '0.01em' },
          xs: { value: '0.005em' },
          lg: { value: '-0.01em' },
          xl: { value: '-0.01em' },
          '2xl': { value: '-0.015em' },
          '3xl': { value: '-0.02em' },
          '4xl': { value: '-0.025em' },
          '5xl': { value: '-0.03em' },
          eyebrow: { value: '0.01em' },
        },
      },

      semanticTokens: {
        colors: {
          bg: { DEFAULT: t('#060f19', '{colors.ice.50}'), rail: t('#0a131e', '#ffffff') },
          surface: {
            DEFAULT: t('#111a25', '#ffffff'),
            2: t('#1a242f', '{colors.ice.100}'),
            3: t('#232e39', '#ffffff'),
            code: t('#020912', '{colors.ice.100}'),
          },
          border: {
            soft: t('#28333f', '{colors.ice.200}'),
            DEFAULT: t('#3b4653', '#c8ced6'),
            strong: t('{colors.ice.400}', '{colors.ice.500}'),
          },
          fg: {
            DEFAULT: t('{colors.ice.100}', '{colors.ice.975}'),
            muted: t('{colors.ice.300}', '{colors.ice.700}'),
            soft: t('{colors.ice.400}', '{colors.ice.600}'),
          },
          action: {
            DEFAULT: t('{colors.beak.400}', '{colors.floe.800}'),
            hover: t('{colors.beak.300}', '{colors.floe.900}'),
            fg: t('{colors.ice.975}', '#ffffff'),
            text: t('{colors.beak.300}', '{colors.beak.800}'),
            subtle: t('{colors.beak.950}', '{colors.beak.100}'),
          },
          live: {
            DEFAULT: t('{colors.aurora.400}', '{colors.aurora.700}'),
            hover: t('{colors.aurora.300}', '{colors.aurora.800}'),
            fg: t('{colors.ice.975}', '#ffffff'),
            text: t('{colors.aurora.300}', '{colors.aurora.800}'),
            subtle: t('{colors.aurora.950}', '{colors.aurora.50}'),
          },
          danger: {
            DEFAULT: { value: '{colors.krill.600}' },
            hover: { value: '{colors.krill.700}' },
            fg: { value: '#ffffff' },
            text: t('{colors.krill.300}', '{colors.krill.700}'),
            subtle: t('{colors.krill.950}', '{colors.krill.50}'),
          },
          warning: {
            DEFAULT: t('{colors.ember.400}', '{colors.ember.500}'),
            text: t('{colors.ember.300}', '{colors.ember.700}'),
            subtle: t('{colors.ember.950}', '{colors.ember.50}'),
          },
          success: {
            DEFAULT: t('{colors.moss.400}', '{colors.moss.600}'),
            text: t('{colors.moss.300}', '{colors.moss.700}'),
            subtle: t('{colors.moss.950}', '{colors.moss.50}'),
          },
          info: {
            DEFAULT: t('{colors.floe.400}', '{colors.floe.600}'),
            text: t('{colors.floe.300}', '{colors.floe.700}'),
            subtle: t('{colors.floe.950}', '{colors.floe.50}'),
          },
          focus: t('{colors.floe.300}', '{colors.floe.600}'),
          presence: {
            available: t('{colors.moss.400}', '{colors.moss.600}'),
            away: t('{colors.beak.400}', '{colors.beak.600}'),
            dnd: t('{colors.krill.400}', '{colors.krill.600}'),
            offline: t('{colors.ice.400}', '{colors.ice.500}'),
          },
          // Gold is the neck patch: "this is for you" (mentions, unread).
          gold: {
            DEFAULT: { value: '{colors.beak.400}' },
            fg: { value: '{colors.ice.975}' },
            text: t('{colors.beak.300}', '{colors.beak.800}'),
            subtle: t('{colors.beak.950}', '{colors.beak.100}'),
          },
          halo: t('{colors.floe.200}', '{colors.floe.700}'),
          mist: t('{colors.ice.300}', '{colors.ice.700}'),
          sea: {
            DEFAULT: t('{colors.floe.950}', '{colors.floe.50}'),
            deep: t('{colors.floe.975}', '{colors.floe.100}'),
            shallow: t('{colors.floe.900}', '{colors.floe.200}'),
          },
          avatar: {
            1: { value: '{colors.beak.200}' },
            2: { value: '{colors.floe.200}' },
            3: { value: '{colors.ice.200}' },
            4: { value: '{colors.aurora.200}' },
            5: { value: '{colors.krill.200}' },
            6: { value: '{colors.moss.200}' },
            fg: { value: '{colors.ice.975}' },
          },
          scrim: t('rgba(2, 9, 18, 0.72)', 'rgba(6, 15, 25, 0.5)'),
          selection: t('rgba(120, 181, 249, 0.32)', 'rgba(158, 204, 255, 0.4)'),
          material: {
            DEFAULT: t('color-mix(in srgb, {colors.surface} 76%, transparent)', 'color-mix(in srgb, {colors.surface} 80%, transparent)'),
            media: { value: 'rgba(7, 12, 18, 0.72)' },
          },
          // State layers: one tint for any surface, either theme.
          state: {
            hover: t('color-mix(in srgb, {colors.fg} 6%, transparent)'),
            press: t('color-mix(in srgb, {colors.fg} 10%, transparent)'),
            selected: t('color-mix(in srgb, {colors.fg} 10%, transparent)'),
          },
          // Border colour that pairs with each elevation (shadows.elev.N).
          elev: {
            1: t('transparent', '{colors.border.soft}'),
            2: t('{colors.border}', '{colors.border.soft}'),
            3: t('{colors.border}', 'transparent'),
          },
          // Penguin illustration colours; identical in both themes.
          belly: { value: '#ffffff' },
          ink: { value: '#14181e' },
          beak: { DEFAULT: { value: '#ff8a3d' } },
        },
        shadows: {
          edgeLight: t('inset 0 1px 0 rgba(255, 255, 255, 0.06)', 'inset 0 1px 0 rgba(255, 255, 255, 0.6)'),
          // Night: light edge and border; shadows are supporting only. Daylight: soft, wide shadows.
          elev: {
            1: t('{shadows.edgeLight}, 0 1px 2px rgba(0, 0, 0, 0.4)', '0 1px 2px rgba(11, 14, 19, 0.06)'),
            2: t(
              '{shadows.edgeLight}, 0 12px 28px -10px rgba(0, 0, 0, 0.7), 0 2px 6px rgba(0, 0, 0, 0.35)',
              '0 12px 28px -12px rgba(11, 14, 19, 0.2), 0 2px 6px rgba(11, 14, 19, 0.06)',
            ),
            3: t('{shadows.edgeLight}, 0 24px 56px -12px rgba(0, 0, 0, 0.7)', '0 24px 56px -16px rgba(11, 14, 19, 0.28), 0 2px 8px rgba(11, 14, 19, 0.08)'),
          },
          overlay: t('{shadows.elev.3}'),
          focusRing: t('0 0 0 2px {colors.bg}, 0 0 0 4px {colors.focus}'),
        },
        gradients: {
          wash: {
            aurora: t('radial-gradient(120% 70% at 50% -10%, rgba(78, 215, 223, 0.16), transparent 60%)', 'radial-gradient(120% 70% at 50% -10%, rgba(133, 229, 234, 0.22), transparent 60%)'),
            floe: t('linear-gradient(180deg, #041a32 0%, #060f19 100%)', 'linear-gradient(180deg, #f1f8ff 0%, #f8fafd 100%)'),
            beak: t('radial-gradient(80% 60% at 50% 0%, rgba(255, 193, 57, 0.14), transparent 70%)', 'radial-gradient(80% 60% at 50% 0%, rgba(255, 224, 169, 0.35), transparent 70%)'),
          },
          sheen: t('linear-gradient(180deg, rgba(255, 255, 255, 0.22), rgba(255, 255, 255, 0) 55%)', 'linear-gradient(180deg, rgba(255, 255, 255, 0.16), rgba(255, 255, 255, 0) 55%)'),
          sea: t('radial-gradient(120% 80% at 50% 0%, {colors.sea.shallow}, {colors.sea} 55%, {colors.sea.deep})'),
        },
        // Density: comfortable is the default; touch always gets 44px rows and controls.
        sizes: {
          rowH: { value: { base: '36px', _compact: { base: '32px', _coarse: '44px' }, _roomy: { base: '40px', _coarse: '44px' }, _coarse: '44px' } },
          chipH: { value: { base: '32px', _compact: { base: '28px', _coarse: '44px' }, _roomy: { base: '32px', _coarse: '44px' }, _coarse: '44px' } },
          controlH: { value: { base: '40px', _compact: { base: '36px', _coarse: '44px' }, _roomy: { base: '44px', _coarse: '44px' }, _coarse: '44px' } },
          msgAvatar: { value: { base: '36px', _compact: '28px', _roomy: '40px' } },
        },
        spacing: {
          msgPadY: { value: { base: '6px', _compact: '2px', _roomy: '10px' } },
          gutter: { value: { base: '24px', _narrow: '16px' } },
        },
      },

      textStyles: {
        hero: { value: { ...font('display', 800, '5xl', '5xl', '5xl'), textWrap: 'balance' } },
        display: { value: { ...font('display', 800, '4xl', '4xl', '4xl'), textWrap: 'balance' } },
        headline: { value: { ...font('display', 750, '3xl', '3xl', '3xl'), textWrap: 'balance' } },
        title: { value: { ...font('display', 700, 'xl', 'xl', 'xl'), textWrap: 'balance' } },
        subtitle: { value: font('display', 700, 'lg', 'lg', 'lg') },
        lead: { value: { ...font('body', 400, 'step-1', '1.5'), textWrap: 'pretty' } },
        body: { value: font('body', 400, 'md', 'md') },
        // Chat roles
        message: { value: { ...font('body', 400, 'base', 'base'), textWrap: 'pretty', _night: { letterSpacing: '0.003em' } } },
        author: { value: font('body', 700, 'md', '20px') },
        timestamp: { value: font('body', 400, 'xs', 'xs') },
        channel: { value: font('display', 700, 'lg', 'lg') },
        sidebar: { value: font('body', 500, 'md', '20px') },
        sidebarSection: { value: font('body', 700, 'xs', 'xs') },
        badge: { value: font('body', 700, '2xs', '1', '2xs') },
        tag: { value: font('body', 600, '2xs', '1', '2xs') },
        button: { value: font('body', 600, 'sm', '1') },
        // ≥16px on touch: no iOS focus zoom.
        input: { value: { ...font('body', 400, 'md', 'md'), _coarse: { fontSize: 'base', lineHeight: 'base' } } },
        label: { value: font('body', 600, 'sm', 'sm') },
        caption: { value: { ...font('body', 400, 'xs', 'xs', 'xs'), fontVariantNumeric: 'tabular-nums' } },
        eyebrow: { value: { ...font('body', 700, 'xs', 'xs', 'eyebrow'), color: 'fg.muted' } },
        code: { value: font('mono', 400, 'sm', 'sm') },
        // Marketing: fluid sizes on the same families.
        marketing: {
          hero: { value: { ...font('display', 800, 'step-6', '1', '-0.03em'), textWrap: 'balance' } },
          heroXl: { value: { ...font('display', 800, 'step-7', '0.95', '-0.035em'), fontStretch: '87.5%', textWrap: 'balance' } },
          display: { value: { ...font('display', 800, 'step-4', '1.1', '-0.025em'), textWrap: 'balance' } },
          headline: { value: { ...font('display', 750, 'step-3', '1.15', '-0.02em'), textWrap: 'balance' } },
          title: { value: { ...font('display', 700, 'step-2', '1.2', '-0.015em'), textWrap: 'balance' } },
          body: { value: font('body', 400, 'step-0', '1.6') },
          caption: { value: { ...font('body', 400, 'step--1', 'xs', 'xs'), fontVariantNumeric: 'tabular-nums' } },
        },
      },

      // Distances read the --wd-* motion vars, so reduced motion turns them into fades.
      keyframes: {
        'wd-waddle': {
          '0%, 100%': { transform: 'translateX(-1.5px) rotate(-12deg)' },
          '50%': { transform: 'translateX(1.5px) translateY(-2px) rotate(12deg)' },
        },
        'wd-hop': { '0%, 60%, 100%': { transform: 'none' }, '30%': { transform: 'translateY(-4px)' } },
        'wd-rise': { from: { opacity: 0, translate: '0 var(--wd-rise)' } },
        'wd-send': { from: { opacity: 0, translate: '0 calc(var(--wd-rise) * 1.5)', scale: 'var(--wd-settle-from)' } },
        'wd-pop': { from: { opacity: 0, scale: 'var(--wd-pop-from)' } },
        'wd-bump': { '40%': { scale: 'calc(1 + (1 - var(--wd-pop-from)) * 0.5)' } },
        'wd-roll': { from: { opacity: 0, translate: '0 calc(var(--wd-rise) * 0.75)' } },
        'wd-grow': { from: { scale: '1 var(--wd-grow-from)' } },
        'wd-tab': { from: { scale: 'var(--wd-grow-from) 1' } },
        'wd-dialog': { from: { opacity: 0, translate: '0 var(--wd-rise)', scale: 'var(--wd-settle-from)' } },
        'wd-toast': { from: { opacity: 0, translate: '0 calc(var(--wd-rise) * 2)', scale: 'var(--wd-settle-from)' } },
        'wd-out': { to: { opacity: 0, translate: '0 calc(var(--wd-rise) * 0.5)' } },
        'wd-fade': { from: { opacity: 0 } },
        'wd-slide': { from: { opacity: 0, translate: 'var(--wd-slide) 0' } },
        'wd-join': {
          from: { opacity: 0, scale: 'calc(1 - (1 - var(--wd-pop-from)) * 0.25)', boxShadow: '0 0 0 0 {colors.live}' },
          '60%': { boxShadow: '0 0 0 4px {colors.live}' },
        },
        'wd-flash': { from: { backgroundColor: 'color-mix(in srgb, {colors.gold} 35%, transparent)' } },
        'wd-pulse': {
          '0%': { boxShadow: '0 0 0 0 color-mix(in srgb, {colors.live} 55%, transparent)' },
          '70%, 100%': { boxShadow: '0 0 0 7px color-mix(in srgb, {colors.live} 0%, transparent)' },
        },
        'wd-ripple': {
          '0%': { boxShadow: '0 0 0 0 color-mix(in srgb, {colors.live} 60%, transparent)' },
          '100%': { boxShadow: '0 0 0 6px color-mix(in srgb, {colors.live} 0%, transparent)' },
        },
        'wd-speak': { to: { boxShadow: '0 0 0 6px color-mix(in srgb, {colors.live} 45%, transparent)' } },
        'wd-shimmer': { to: { backgroundPosition: '-150% 0' } },
        'wd-bob': { '0%, 100%': { transform: 'translateY(0) rotate(0)' }, '50%': { transform: 'translateY(-4px) rotate(-1.2deg)' } },
      },

      // Parts keep their bundle.css class names (wd-msg__body, wd-dialog__head, …)
      // and are styled from the block recipe, so board markup ports unchanged.
      recipes: {
        row: { className: 'wd-row', base: { display: 'flex', flexWrap: 'wrap', gap: '2', alignItems: 'center', p: '4' } },
        stackBox: { className: 'wd-stack', base: { display: 'flex', flexDirection: 'column', gap: '2', p: '4' } },
        focus: {
          className: 'wd-focus',
          base: { outline: '{borderWidths.focus} solid {colors.focus}', outlineOffset: '2px', '@media (forced-colors: active)': { outlineColor: 'Highlight' } },
        },

        // 40px tall (control-h), 36px small; 44px on touch.
        btn: {
          className: 'wd-btn',
          base: {
            display: 'inline-flex',
            alignItems: 'center',
            gap: '2',
            height: 'controlH',
            minHeight: 'controlH',
            px: '4',
            borderRadius: 'control',
            border: '1px solid {colors.border}',
            bg: 'surface',
            color: 'fg',
            textStyle: 'button',
            cursor: 'pointer',
            transition,
            '& svg': {
              ...icon('20px'),
              flex: 'none',
              transition: 'translate {durations.base} {easings.spring.bouncy}, rotate {durations.base} {easings.spring.bouncy}',
            },
            _hover: { bg: 'surface.2', borderColor: 'border.strong', bgImage: layer('hover') },
            _active: { ...press, bgImage: layer('press') },
            ...btnDisabled,
            ...forcedBorder,
          },
          variants: {
            variant: {
              primary: btnTone('action'),
              danger: btnTone('danger'),
              live: { ...btnTone('live'), borderRadius: 'pill', _active: { animation: 'wd-ripple 600ms {easings.out}' } },
            },
            size: {
              sm: { height: '36px', minHeight: '36px', px: '3', fontSize: 'sm', '& svg': icon('16px') },
              lg: { height: '48px', minHeight: '48px', px: '5', fontSize: 'md' },
            },
            icon: { true: { width: '40px', px: '0', justifyContent: 'center' } },
          },
        },

        iconbtn: {
          className: 'wd-iconbtn',
          base: {
            display: 'inline-flex',
            ...icon('40px'),
            flex: 'none',
            alignItems: 'center',
            justifyContent: 'center',
            border: 0,
            borderRadius: 'control',
            bg: 'transparent',
            color: 'fg.muted',
            cursor: 'pointer',
            transition,
            '& svg': { ...icon('20px'), transition: 'translate {durations.base} {easings.spring.bouncy}, rotate {durations.base} {easings.spring.bouncy}' },
            _hover: { bg: 'state.hover', color: 'fg', '& svg': { translate: '0 calc(var(--wd-nudge) * -1)' } },
            _active: press,
            _pressed: { bg: 'state.selected', color: 'fg' },
          },
          variants: {
            variant: {
              // Send sleeps until the composer has text.
              send: {
                bg: 'action',
                color: 'action.fg',
                bgImage: sheen,
                boxShadow: raised,
                transition: 'background-color {durations.base} {easings.out}, color {durations.base} {easings.out}',
                _hover: {
                  bg: 'action.hover',
                  color: 'action.fg',
                  bgImage: `${sheen}, ${layer('hover')}`,
                  '& svg': { translate: 'var(--wd-nudge) calc(var(--wd-nudge) * -1)', rotate: '-8deg' },
                },
                _active: { '& svg': { translate: 'calc(var(--wd-nudge) * 3) calc(var(--wd-nudge) * -3)', transitionDuration: 'instant' } },
                '.wd-composer:has(textarea:placeholder-shown) &': { bg: 'transparent', bgImage: 'none', boxShadow: 'none', color: 'fg.soft' },
              },
            },
            size: { sm: { ...icon('32px'), '& svg': icon('16px') } },
          },
        },

        // An input, so its edge meets 3:1.
        composer: {
          className: 'wd-composer',
          base: {
            '--r-out': '{radii.panel}',
            '--pad': '6px',
            display: 'flex',
            alignItems: 'flex-end',
            gap: '1',
            padding: '6px',
            border: '1px solid {colors.border.strong}',
            borderRadius: 'panel',
            bg: 'surface',
            boxShadow: 'elev.1',
            transition,
            '& textarea': {
              flex: 1,
              minWidth: 0,
              resize: 'none',
              border: 0,
              bg: 'transparent',
              color: 'fg',
              font: '16px/1.5 {fonts.body}',
              padding: '8px {spacing.2}',
              outline: 'none',
            },
            '& textarea::placeholder': { color: 'fg.soft' },
            '& .wd-iconbtn': { borderRadius: concentric },
            _focusWithin: { outline: '3px solid {colors.focus}', outlineOffset: '1px', boxShadow: `${focusHalo}, {shadows.elev.2}` },
            ...forcedBorder,
          },
        },

        // A visible label, a 3:1 edge and a 44px target.
        field: {
          className: 'wd-field',
          base: { display: 'flex', flexDirection: 'column', gap: '1.5', '& label': { textStyle: 'label', color: 'fg' } },
        },
        input: {
          className: 'wd-input',
          base: {
            minHeight: '44px',
            height: '44px',
            boxSizing: 'border-box',
            px: '3',
            borderRadius: 'control',
            border: '1px solid {colors.border.strong}',
            bg: 'surface',
            color: 'fg',
            textStyle: 'input',
            transition,
            _placeholder: { color: 'fg.soft' },
            _hover: { borderColor: 'fg.soft' },
            '&[aria-invalid="true"]': { borderColor: 'danger.text', boxShadow: 'inset 0 0 0 1px {colors.danger.text}' },
            _focus: { outline: '3px solid {colors.focus}', outlineOffset: '1px', borderColor: 'border.strong', boxShadow: focusHalo },
            ...forcedBorder,
          },
        },

        tag: {
          className: 'wd-tag',
          base: {
            display: 'inline-flex',
            alignItems: 'center',
            height: '22px',
            px: '2',
            borderRadius: 'tag',
            border: '1px solid {colors.border}',
            color: 'fg.muted',
            textStyle: 'tag',
            ...forcedBorder,
          },
        },
        kicker: { className: 'wd-kicker', base: { textStyle: 'eyebrow' } },

        // The gold neck patch. Gold means "this is for you": mentions and unread.
        badge: {
          className: 'wd-badge',
          base: {
            display: 'inline-flex',
            alignItems: 'center',
            justifyContent: 'center',
            minWidth: '20px',
            height: '20px',
            px: '1.5',
            boxSizing: 'border-box',
            borderRadius: 'pill',
            bg: 'gold',
            color: 'gold.fg',
            textStyle: 'badge',
            fontVariantNumeric: 'tabular-nums',
            animation: 'wd-pop {durations.slower} {easings.spring.bouncy} backwards',
            ...forcedBorder,
          },
          variants: {
            variant: { quiet: { bg: 'surface.2', color: 'fg.muted', boxShadow: 'inset 0 0 0 1px {colors.border}' } },
            dot: { true: { minWidth: '8px', width: '8px', height: '8px', p: '0' } },
            bump: { true: { animation: 'wd-bump {durations.slow} {easings.spring.bouncy}' } },
          },
        },

        // Every presence state has its own shape, never colour alone.
        avatar: {
          className: 'wd-avatar',
          base: {
            position: 'relative',
            display: 'inline-flex',
            ...icon('36px'),
            flex: 'none',
            borderRadius: 'pill',
            alignItems: 'center',
            justifyContent: 'center',
            bg: 'surface.2',
            color: 'fg',
            font: '700 13px/1 {fonts.body}',
            '&[data-speaking="true"]': {
              boxShadow: '0 0 0 3px {colors.live}',
              animation: 'wd-speak 1.2s {easings.inOut} infinite alternate',
              _motionReduce: { animation: 'none' },
              '@media (forced-colors: active)': { outline: '3px solid Highlight' },
            },
          },
          variants: {
            size: {
              xs: { ...icon('20px'), fontSize: '10px' },
              sm: { ...icon('28px'), fontSize: '12px', '& .wd-presence': { ...icon('12px'), right: '-3px', bottom: '-3px' } },
              lg: { ...icon('44px'), fontSize: '16px' },
              xl: { ...icon('64px'), fontSize: '22px' },
            },
            tint: Object.fromEntries([1, 2, 3, 4, 5, 6].map((n) => [n, { bg: `avatar.${n}`, color: 'avatar.fg' }])),
            space: { true: { borderRadius: 'space' } },
          },
        },
        presence: {
          className: 'wd-presence',
          base: {
            position: 'absolute',
            right: '-2px',
            bottom: '-2px',
            ...icon('14px'),
            borderRadius: 'pill',
            border: '2px solid var(--ring-bg, {colors.bg})',
            boxSizing: 'border-box',
            '&[data-fresh]': { animation: 'wd-ripple 900ms {easings.out}' },
          },
          variants: {
            state: {
              available: { bg: 'presence.available' },
              away: {
                background: 'linear-gradient(90deg, {colors.presence.away} 50%, var(--ring-bg, {colors.bg}) 50%)',
                boxShadow: 'inset 0 0 0 2px {colors.presence.away}',
              },
              dnd: {
                bg: 'presence.dnd',
                _after: { content: '""', position: 'absolute', left: '2px', right: '2px', top: '4px', height: '2px', borderRadius: '1px', background: 'var(--ring-bg, {colors.bg})' },
              },
              offline: { background: 'var(--ring-bg, {colors.bg})', boxShadow: 'inset 0 0 0 2px {colors.presence.offline}' },
            },
          },
        },

        // Room list. Neutral selection plus keel; gold stays "for you".
        rail: {
          className: 'wd-rail',
          base: {
            '--ring-bg': '{colors.bg.rail}',
            width: 'sidebar',
            bg: 'bg.rail',
            p: '2',
            borderRadius: 'panel',
            display: 'flex',
            flexDirection: 'column',
            gap: '0.5',
            '& .wd-kicker': { padding: '{spacing.3} {spacing.3} {spacing.1}' },
          },
        },
        room: {
          className: 'wd-room',
          base: {
            position: 'relative',
            display: 'flex',
            alignItems: 'center',
            gap: '2.5',
            minHeight: 'rowH',
            px: '2.5',
            borderRadius: 'control',
            color: 'fg.muted',
            textDecoration: 'none',
            textStyle: 'sidebar',
            transition,
            '& svg': { ...icon('20px'), flex: 'none', color: 'fg.soft' },
            '& .wd-room__name': { flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' },
            _hover: { bg: 'state.hover', color: 'fg', '& svg': { color: 'fg' } },
            '&[data-unread]': { color: 'fg', fontWeight: 700 },
            _currentPage: {
              bg: 'state.selected',
              color: 'fg',
              fontWeight: 700,
              '& svg': { color: 'fg' },
              _before: {
                content: '""',
                position: 'absolute',
                left: 0,
                top: '8px',
                bottom: '8px',
                width: '3px',
                borderRadius: '0 3px 3px 0',
                bg: 'fg',
                animation: 'wd-grow {durations.slow} {easings.spring.snappy} backwards',
                '@media (forced-colors: active)': { background: 'Highlight', forcedColorAdjust: 'none' },
              },
            },
          },
        },

        // Message rows (rooms).
        msg: {
          className: 'wd-msg',
          base: {
            position: 'relative',
            display: 'grid',
            gridTemplateColumns: '{sizes.msgAvatar} 1fr',
            gap: '0 {spacing.3}',
            padding: '{spacing.msgPadY} {spacing.4}',
            borderRadius: 'control',
            transition,
            _hover: { bg: 'state.hover' },
            '& > .wd-avatar:first-child:not([class*="--size_"])': { width: 'msgAvatar', height: 'msgAvatar' },
            '& .wd-msg__meta': { display: 'flex', alignItems: 'baseline', gap: '2' },
            '& .wd-msg__author': { fontWeight: 700, color: 'fg' },
            '& .wd-msg__time': { textStyle: 'timestamp', color: 'fg.muted', fontVariantNumeric: 'tabular-nums' },
            '& .wd-msg__body': { gridColumn: 2, textStyle: 'message', color: 'fg', minWidth: 0, overflowWrap: 'anywhere' },
            '& code': { fontFamily: 'mono', fontSize: '0.875em', fontWeight: 400, bg: 'surface.code', padding: '1px 5px', borderRadius: 'tag' },
            '& .wd-msg__actions': {
              position: 'absolute',
              top: '-16px',
              right: '4',
              zIndex: 'dropdown',
              display: 'flex',
              gap: '0.5',
              p: '0.5',
              borderRadius: 'control',
              bg: 'surface',
              border: '1px solid {colors.elev.2}',
              boxShadow: 'elev.2',
              opacity: 0,
              translate: '0 4px',
              pointerEvents: 'none',
              transition: 'opacity {durations.fast} {easings.out}, translate {durations.fast} {easings.out}',
              _motionReduce: { translate: '0 0' },
            },
            '& .wd-msg__actions .wd-iconbtn': { '--r-out': '{radii.control}', '--pad': '2px', ...icon('32px'), borderRadius: concentric },
            '&:hover .wd-msg__actions, &:focus-within .wd-msg__actions': { opacity: 1, translate: '0 0', pointerEvents: 'auto' },
          },
          variants: {
            cont: { true: { pt: '0' } },
            mention: { true: { bg: 'gold.subtle', boxShadow: 'inset 3px 0 0 {colors.gold}' } },
            target: { true: { animation: 'wd-flash 1.6s {easings.out}' } },
            new: { true: { animation: 'wd-send {durations.slow} {easings.spring.snappy} backwards' } },
          },
        },
        reactions: { className: 'wd-reactions', base: { gridColumn: 2, display: 'flex', gap: '1', mt: '1', flexWrap: 'wrap' } },
        mention: {
          className: 'wd-mention',
          base: { color: 'gold.text', bg: 'gold.subtle', fontWeight: 700, padding: '0 3px', borderRadius: 'tag' },
        },
        code: {
          className: 'wd-code',
          base: { fontFamily: 'mono', fontSize: '0.875em', fontWeight: 400, bg: 'surface.code', padding: '1px 5px', borderRadius: 'tag' },
        },
        chip: {
          className: 'wd-chip',
          base: {
            display: 'inline-flex',
            alignItems: 'center',
            gap: '1',
            height: 'chipH',
            minHeight: 'chipH',
            padding: '0 10px',
            borderRadius: 'pill',
            border: '1px solid {colors.border}',
            bg: 'surface',
            fontSize: 'xs',
            color: 'fg.muted',
            fontVariantNumeric: 'tabular-nums',
            transition,
            '& svg': icon('16px'),
            '& .wd-chip__n': { display: 'inline-block', animation: 'wd-roll {durations.base} {easings.out} backwards' },
            _hover: { borderColor: 'border.strong', color: 'fg', bgImage: layer('hover') },
            _active: { ...press, bgImage: layer('press') },
            _pressed: {
              bg: 'action.subtle',
              borderColor: 'action.text',
              color: 'fg',
              fontWeight: 700,
              boxShadow: 'inset 0 0 0 1px {colors.action.text}',
              animation: 'wd-pop {durations.slower} {easings.spring.bouncy} backwards',
            },
            ...forcedBorder,
          },
        },

        // Bubbles (direct messages). Three round corners, one tail corner on the speaker's side.
        bubbles: { className: 'wd-bubbles', base: { display: 'flex', flexDirection: 'column', gap: '1', p: '4' } },
        bubble: {
          className: 'wd-bubble',
          base: {
            maxWidth: '72%',
            padding: '10px 14px',
            borderRadius: tail,
            bg: 'surface.2',
            color: 'fg',
            fontSize: 'base',
            lineHeight: '1.45',
            alignSelf: 'flex-start',
            '& .wd-bubble__time': { display: 'block', mt: '0.5', fontSize: '2xs', opacity: 0.8, textAlign: 'right', fontVariantNumeric: 'tabular-nums' },
          },
          variants: {
            own: {
              true: {
                alignSelf: 'flex-end',
                bg: 'action',
                color: 'action.fg',
                borderRadius: '{radii.bubble} {radii.bubble} {radii.tag} {radii.bubble}',
              },
            },
            new: { true: { animation: 'wd-send {durations.slow} {easings.spring.snappy} backwards' } },
          },
          compoundVariants: [{ own: true, new: true, css: { transformOrigin: '100% 100%' } }],
        },

        // Typing: the waddle. Dots rock side to side like a penguin walking.
        typing: { className: 'wd-typing', base: { display: 'inline-flex', alignItems: 'center', gap: '2', fontSize: 'xs', color: 'fg.muted' } },
        waddle: {
          className: 'wd-waddle',
          base: {
            display: 'inline-flex',
            gap: '1',
            padding: '9px 11px',
            borderRadius: tail,
            bg: 'surface.2',
            '& i': {
              ...icon('7px'),
              borderRadius: 'pill',
              bg: 'fg.muted',
              animation: 'wd-waddle 1.2s ease-in-out infinite',
              _motionReduce: { animation: 'none !important' },
            },
            '& i:nth-child(2)': { animationDelay: '.15s' },
            '& i:nth-child(3)': { animationDelay: '.3s' },
          },
        },

        // Empty state: the one place the penguin talks (and breathes).
        empty: {
          className: 'wd-empty',
          base: {
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            textAlign: 'center',
            gap: '2',
            p: '6',
            maxWidth: '360px',
            mx: 'auto',
            '& img': {
              ...icon('120px'),
              animation: 'wd-bob 4.8s ease-in-out 2',
              transformOrigin: '50% 100%',
              _motionReduce: { animation: 'none !important' },
            },
            '& h3': { margin: '{spacing.2} 0 0', ...font('display', 800, 'xl', 'xl', 'xl'), color: 'fg', textWrap: 'balance' },
            '& p': { margin: 0, color: 'fg.muted', textWrap: 'pretty' },
          },
        },

        // Sea backdrop + sign-in card (waddle.social, colony).
        sea: {
          className: 'wd-sea',
          base: { bgImage: '{gradients.sea}', padding: '{spacing.8} {spacing.4}', display: 'flex', justifyContent: 'center' },
        },
        card: {
          className: 'wd-card',
          base: {
            '--ring-bg': '{colors.surface}',
            width: '100%',
            maxWidth: '380px',
            bg: 'surface',
            border: '1px solid {colors.border}',
            borderRadius: 'card',
            p: '6',
            display: 'flex',
            flexDirection: 'column',
            gap: '4',
            boxSizing: 'border-box',
            boxShadow: 'elev.1',
            '& .wd-card__mark': icon('48px'),
            '& h2': { margin: 0, ...font('display', 800, '3xl', '3xl', '3xl'), textWrap: 'balance' },
            '& p': { margin: 0, color: 'fg.muted' },
            '& .wd-btn': { width: '100%', justifyContent: 'center', height: '44px' },
            ...lift,
          },
        },

        // The standard card.
        floe: {
          className: 'wd-floe',
          base: {
            '--r-out': '{radii.panel}',
            '--pad': '{spacing.5}',
            '--ring-bg': '{colors.surface}',
            display: 'flex',
            flexDirection: 'column',
            gap: '4',
            padding: 'var(--pad)',
            borderRadius: 'var(--r-out)',
            bg: 'surface',
            border: '1px solid {colors.elev.1}',
            color: 'fg',
            textDecoration: 'none',
            boxShadow: 'elev.1',
            transition,
            ...lift,
            ...forcedBorder,
          },
          variants: {
            list: {
              true: {
                '--pad': '0px',
                gap: '0',
                overflow: 'hidden',
                '& > * + *': { borderTop: '1px solid {colors.border.soft}' },
                '& > * > a, & > a': {
                  display: 'flex',
                  alignItems: 'center',
                  gap: '3',
                  padding: '{spacing.3} {spacing.4}',
                  color: 'fg',
                  textDecoration: 'none',
                },
                '& a:hover': { bg: 'state.hover' },
              },
            },
          },
        },

        // Dividers (date, new messages).
        separator: {
          className: 'wd-divider',
          base: {
            display: 'flex',
            alignItems: 'center',
            gap: '3',
            padding: '{spacing.2} {spacing.4}',
            fontSize: 'xs',
            color: 'fg.muted',
            '&::before, &::after': { content: '""', flex: 1, height: '1px', bg: 'border.soft' },
          },
          variants: {
            new: { true: { color: 'gold.text', fontWeight: 700, '&::before, &::after': { bg: 'gold' } } },
          },
        },

        // Tabs (underline).
        tabs: { className: 'wd-tabs', base: { display: 'flex', gap: '1', borderBottom: '1px solid {colors.border.soft}' } },
        tab: {
          className: 'wd-tab',
          base: {
            position: 'relative',
            display: 'inline-flex',
            alignItems: 'center',
            gap: '2',
            height: '40px',
            px: '3',
            border: 0,
            borderRadius: '{radii.control} {radii.control} 0 0',
            bg: 'transparent',
            color: 'fg.muted',
            font: '600 14px/1 {fonts.body}',
            textDecoration: 'none',
            cursor: 'pointer',
            transition,
            _hover: { bg: 'state.hover' },
            _active: press,
            '&:is([aria-selected="true"], [aria-current="page"])': {
              color: 'fg',
              _after: {
                content: '""',
                position: 'absolute',
                left: '3',
                right: '3',
                bottom: '-1px',
                height: '2px',
                borderRadius: '2px',
                bg: 'fg',
                animation: 'wd-tab {durations.base} {easings.inOut} backwards',
                '@media (forced-colors: active)': { background: 'Highlight', forcedColorAdjust: 'none' },
              },
            },
          },
        },

        // Segmented control (capsule in capsule).
        seg: {
          className: 'wd-seg',
          base: {
            display: 'inline-flex',
            gap: '0.5',
            padding: '3px',
            borderRadius: 'pill',
            bg: 'surface.2',
            '& > *': {
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              gap: '1.5',
              height: '32px',
              px: '3',
              border: 0,
              borderRadius: 'pill',
              bg: 'transparent',
              color: 'fg.muted',
              font: '600 13px/1 {fonts.body}',
              textDecoration: 'none',
              cursor: 'pointer',
              transition,
            },
            '& > *:hover': { color: 'fg' },
            '& > :is([aria-pressed="true"], [aria-selected="true"], [aria-checked="true"], [aria-current="page"])': {
              bg: 'surface',
              color: 'fg',
              boxShadow: '0 0 0 1px {colors.border}, {shadows.elev.1}',
            },
            '& svg': icon('16px'),
            ...forcedBorder,
          },
          variants: {
            size: { lg: { '& > *': { height: '38px', minWidth: '38px' } } },
          },
        },

        // Option cards (radio-like choices).
        option: {
          className: 'wd-option',
          base: {
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'flex-start',
            gap: '1',
            p: '3',
            borderRadius: 'control',
            border: '1px solid {colors.border}',
            bg: 'surface',
            color: 'fg',
            font: 'inherit',
            textAlign: 'left',
            cursor: 'pointer',
            transition,
            _hover: { bgImage: layer('hover') },
            _active: press,
            '&:is([aria-pressed="true"], [aria-checked="true"])': { bg: 'state.selected', borderColor: 'fg', boxShadow: 'inset 0 0 0 1px {colors.fg}' },
            ...forcedBorder,
          },
        },

        // Switch: role="switch" + aria-checked; thumb is ::after or a child <i>.
        toggle: {
          className: 'wd-switch',
          base: {
            position: 'relative',
            display: 'inline-block',
            flex: 'none',
            width: '44px',
            height: '26px',
            boxSizing: 'border-box',
            borderRadius: 'pill',
            bg: 'surface.2',
            border: '1px solid {colors.border.strong}',
            cursor: 'pointer',
            transition,
            '&::after, & > i': {
              content: '""',
              position: 'absolute',
              top: '2px',
              left: '2px',
              ...icon('20px'),
              borderRadius: 'pill',
              bg: 'fg.muted',
              transition: 'translate {durations.slow} {easings.spring.snappy}, background-color {durations.base} {easings.out}',
              _motionReduce: { transition: 'background-color {durations.base} {easings.out}' },
              '@media (forced-colors: active)': { background: 'Highlight', forcedColorAdjust: 'none' },
            },
            '&:has(> i)::after': { display: 'none' },
            '&[aria-checked="true"]': { bg: 'action', borderColor: 'action' },
            '&[aria-checked="true"]::after, &[aria-checked="true"] > i': { translate: '18px 0', bg: 'action.fg' },
            ...forcedBorder,
          },
        },

        // Shell pieces.
        header: {
          className: 'wd-header',
          base: {
            position: 'sticky',
            top: 0,
            zIndex: 'sticky',
            display: 'flex',
            alignItems: 'center',
            gap: '3',
            minHeight: 'header',
            boxSizing: 'border-box',
            padding: '0 {spacing.gutter}',
            borderBottom: '1px solid {colors.border.soft}',
            bg: 'bg',
            '&[data-stuck]': {
              bg: 'material',
              WebkitBackdropFilter: glass,
              backdropFilter: glass,
              '@supports not ((backdrop-filter: blur(1px)) or (-webkit-backdrop-filter: blur(1px)))': { bg: 'surface' },
              '@media (prefers-reduced-transparency: reduce)': { bg: 'surface', WebkitBackdropFilter: 'none', backdropFilter: 'none' },
            },
          },
        },
        panel: {
          className: 'wd-panel',
          base: {
            '--ring-bg': '{colors.bg.rail}',
            flex: '0 1 {sizes.panel}',
            minWidth: 'panel.min',
            maxWidth: 'panel',
            display: 'flex',
            flexDirection: 'column',
            borderLeft: '1px solid {colors.border.soft}',
            bg: 'bg.rail',
            animation: 'wd-slide {durations.slow} {easings.out} backwards',
          },
        },
        page: {
          className: 'wd-page',
          base: {
            width: '100%',
            maxWidth: 'page',
            mx: 'auto',
            padding: '{spacing.10} {spacing.gutter}',
            boxSizing: 'border-box',
            display: 'flex',
            flexDirection: 'column',
            gap: '8',
          },
          variants: { form: { true: { maxWidth: 'form' } } },
        },

        dialog: {
          className: 'wd-dialog',
          base: {
            '--r-out': '{radii.card}',
            '--pad': '{spacing.6}',
            '--ring-bg': '{colors.surface}',
            width: 'min({sizes.dialog.md}, 100% - 32px)',
            boxSizing: 'border-box',
            display: 'flex',
            flexDirection: 'column',
            gap: '4',
            padding: 'var(--pad)',
            borderRadius: 'var(--r-out)',
            bg: 'surface',
            border: '1px solid {colors.elev.3}',
            boxShadow: 'elev.3',
            animation: 'wd-dialog {durations.slow} {easings.spring.snappy} backwards',
            '&[data-closing]': { animation: 'wd-out {durations.base} {easings.in} forwards' },
            '& .wd-dialog__head': { display: 'flex', alignItems: 'center', gap: '2' },
            '& .wd-dialog__head h2': { margin: 0, flex: 1, font: '700 20px/1.25 {fonts.display}' },
            '& .wd-dialog__foot': { display: 'flex', justifyContent: 'flex-end', alignItems: 'center', gap: '2', pt: '1' },
            ...forcedBorder,
          },
          variants: {
            size: {
              sm: { width: 'min({sizes.dialog.sm}, 100% - 32px)' },
              lg: { width: 'min({sizes.dialog.lg}, 100% - 32px)' },
            },
          },
        },
        scrim: {
          className: 'wd-scrim',
          base: { position: 'fixed', inset: 0, zIndex: 'scrim', bg: 'scrim', animation: 'wd-fade {durations.base} {easings.out} backwards' },
        },
        toast: {
          className: 'wd-toast',
          base: {
            display: 'flex',
            alignItems: 'center',
            gap: '3',
            minHeight: '48px',
            boxSizing: 'border-box',
            padding: '{spacing.3} {spacing.4}',
            borderRadius: 'panel',
            bg: 'surface',
            color: 'fg',
            border: '1px solid {colors.elev.2}',
            boxShadow: 'elev.3',
            zIndex: 'toast',
            animation: 'wd-toast {durations.slow} {easings.spring.snappy} backwards',
            '&[data-closing]': { animation: 'wd-out {durations.base} {easings.in} forwards' },
            ...forcedBorder,
          },
        },
        skel: {
          className: 'wd-skel',
          base: {
            display: 'block',
            height: '12px',
            borderRadius: 'xs',
            background:
              'linear-gradient(90deg, {colors.surface.2} 30%, color-mix(in srgb, {colors.surface.2} 60%, {colors.fg} 8%) 50%, {colors.surface.2} 70%) 0 0 / 300% 100%',
            animation: 'wd-shimmer 1.6s linear infinite',
            _motionReduce: { animation: 'none !important' },
          },
        },

        // Huddle kit.
        liveDot: {
          className: 'wd-live-dot',
          base: {
            display: 'inline-block',
            flex: 'none',
            ...icon('8px'),
            borderRadius: 'pill',
            bg: 'live',
            animation: 'wd-pulse {durations.ambient} {easings.out} infinite',
            _motionReduce: { animation: 'none !important' },
          },
        },
        huddleCtl: {
          className: 'wd-huddle-ctl',
          base: {
            display: 'inline-flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '1.5',
            height: '44px',
            minWidth: '44px',
            px: '3',
            boxSizing: 'border-box',
            border: 0,
            borderRadius: 'pill',
            bg: 'surface.2',
            color: 'fg',
            font: '600 13px/1 {fonts.body}',
            cursor: 'pointer',
            transition: 'background-color {durations.fast} {easings.out}, scale {durations.base} {easings.spring.snappy}',
            _hover: { bgImage: layer('hover') },
            _active: { scale: 'var(--wd-press)' },
            _pressed: { bg: 'action.subtle', color: 'action.text' },
            '&[data-off="true"]': { bg: 'color-mix(in srgb, {colors.danger} 18%, transparent)', color: 'danger.text' },
          },
        },
        plate: {
          className: 'wd-plate',
          base: {
            display: 'inline-flex',
            alignItems: 'center',
            gap: '1.5',
            maxWidth: 'calc(100% - 16px)',
            height: '26px',
            px: '2.5',
            borderRadius: 'pill',
            bg: 'material.media',
            WebkitBackdropFilter: glass,
            backdropFilter: glass,
            color: '#f3f5f7',
            font: '600 13px/1 {fonts.body}',
            '@supports not ((backdrop-filter: blur(1px)) or (-webkit-backdrop-filter: blur(1px)))': { bg: 'rgba(7, 12, 18, 0.9)' },
            '@media (prefers-reduced-transparency: reduce)': { bg: 'rgba(7, 12, 18, 0.92)', WebkitBackdropFilter: 'none', backdropFilter: 'none' },
          },
        },
        tile: {
          className: 'wd-tile',
          base: {
            position: 'relative',
            borderRadius: 'panel',
            overflow: 'hidden',
            bg: 'sea.deep',
            '&[data-joining]': { animation: 'wd-join {durations.slower} {easings.spring.bouncy} backwards' },
            '&[data-speaking="true"]': {
              boxShadow: '0 0 0 3px {colors.live}',
              animation: 'wd-speak 1.2s {easings.inOut} infinite alternate',
              _motionReduce: { animation: 'none' },
              '@media (forced-colors: active)': { outline: '3px solid Highlight' },
            },
          },
        },

        // Brand device: polar wash, snow dots, film grain. One per screen, marketing and onboarding only.
        polar: {
          className: 'wd-polar',
          base: {
            '--polar-a': 'color-mix(in oklab, {colors.live} 26%, transparent)',
            '--polar-b': 'color-mix(in oklab, {colors.gold} 12%, transparent)',
            '&[data-theme=daylight], [data-theme=daylight] &': {
              '--polar-a': 'color-mix(in oklab, {colors.live} 14%, transparent)',
              '--polar-b': 'color-mix(in oklab, {colors.gold} 22%, transparent)',
            },
            position: 'relative',
            isolation: 'isolate',
            overflow: 'hidden',
            color: 'fg',
            background:
              'radial-gradient(60% 45% at 78% 8%, var(--polar-a), transparent 70%), radial-gradient(45% 35% at 12% 0%, var(--polar-b), transparent 70%), radial-gradient(120% 80% at 50% 0%, {colors.sea.shallow}, {colors.sea} 55%, {colors.sea.deep})',
            _before: {
              content: '""',
              position: 'absolute',
              inset: 0,
              zIndex: -1,
              pointerEvents: 'none',
              background:
                'radial-gradient(circle, color-mix(in oklab, {colors.halo} 55%, transparent) 1px, transparent 1.5px) 0 0/28px 28px, radial-gradient(circle, color-mix(in oklab, {colors.halo} 35%, transparent) .8px, transparent 1.3px) 14px 14px/28px 28px',
              WebkitMaskImage: 'radial-gradient(80% 60% at 50% 0%, #000 0%, transparent 75%)',
              maskImage: 'radial-gradient(80% 60% at 50% 0%, #000 0%, transparent 75%)',
              opacity: 0.5,
            },
            _after: {
              content: '""',
              position: 'absolute',
              inset: 0,
              zIndex: -1,
              pointerEvents: 'none',
              opacity: 0.06,
              mixBlendMode: 'overlay',
              backgroundImage:
                "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='160' height='160'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='.9' numOctaves='2' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E\")",
            },
          },
        },
        floeArt: {
          className: 'wd-floe-art',
          base: {
            display: 'block',
            width: '100%',
            height: 'auto',
            '--floe-top-1': '{colors.belly}',
            '--floe-top-2': '#eef4f8',
            '--floe-top-3': '{colors.avatar.2}',
            '--floe-side-2': '{colors.live}',
            _daylight: { '--floe-side-2': '#9cc9ec' },
          },
        },
        trio: {
          className: 'wd-trio',
          base: {
            display: 'inline-flex',
            gap: '1.5',
            alignItems: 'center',
            '& i': { ...icon('8px'), borderRadius: 'pill', bg: 'gold' },
            '& i:nth-child(2)': { bg: 'live' },
            '& i:nth-child(3)': { bg: 'mist' },
          },
          variants: {
            live: {
              true: {
                '@media (prefers-reduced-motion: no-preference)': {
                  '& i': { animation: 'wd-hop 1.2s {easings.out} 3' },
                  '& i:nth-child(2)': { animationDelay: '.15s' },
                  '& i:nth-child(3)': { animationDelay: '.3s' },
                },
              },
            },
          },
        },
      },
    },
  },
})
