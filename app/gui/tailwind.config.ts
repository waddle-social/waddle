import type { Config } from 'tailwindcss';

export default {
  content: ['./index.html', './src/**/*.{vue,ts,tsx}'],
  theme: {
    extend: {
      colors: {
        background: 'var(--waddle-bg)',
        foreground: 'var(--waddle-fg)',
        accent: 'var(--waddle-accent)',
        surface: 'var(--waddle-surface)',
        'surface-raised': 'var(--waddle-surface-raised)',
        border: 'var(--waddle-border)',
        success: 'var(--waddle-success)',
        warning: 'var(--waddle-warning)',
        danger: 'var(--waddle-error)',
        muted: 'var(--waddle-muted)',
        sidebar: 'var(--waddle-sidebar)',
        header: 'var(--waddle-header)',
        'chat-bg': 'var(--waddle-chat-bg)',
        hover: 'var(--waddle-hover)',
        active: 'var(--waddle-active)',
      },
    },
  },
  plugins: [],
} satisfies Config;
