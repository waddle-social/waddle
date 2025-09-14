export default {
  content: ['./src/**/*.{astro,html,js,jsx,md,mdx,svelte,ts,tsx,vue}'],
  theme: {
    extend: {
      colors: {
        'glass': {
          'bg': 'rgba(255, 255, 255, 0.08)',
          'bg-dark': 'rgba(0, 0, 0, 0.4)',
          'border': 'rgba(255, 255, 255, 0.12)',
          'border-strong': 'rgba(255, 255, 255, 0.2)',
          'hover': 'rgba(255, 255, 255, 0.06)',
          'active': 'rgba(255, 255, 255, 0.1)',
          'accent': 'rgba(255, 255, 255, 0.15)',
          'surface': 'rgba(255, 255, 255, 0.04)',
          'overlay': 'rgba(0, 0, 0, 0.6)',
        },
        'primary': {
          '50': '#f0f9ff',
          '100': '#e0f2fe',
          '200': '#bae6fd',
          '300': '#7dd3fc',
          '400': '#38bdf8',
          '500': '#0ea5e9',
          '600': '#0284c7',
          '700': '#0369a1',
          '800': '#075985',
          '900': '#0c4a6e',
        },
        'accent': {
          'primary': '#0ea5e9',
          'primary-dark': '#0284c7',
          'secondary': '#6b7280',
          'warm': '#f59e0b',
          'green': '#10b981',
          'purple': '#8b5cf6',
        }
      },
      backdropBlur: {
        'xs': '2px',
        'sm': '4px',
        'md': '8px',
        'lg': '16px',
        'xl': '24px',
        '2xl': '40px',
        '3xl': '64px',
      },
      backdropSaturate: {
        '180': '1.8',
        '200': '2',
      },
      borderRadius: {
        '4xl': '2rem',
        '5xl': '2.5rem',
      },
      boxShadow: {
        'glass': '0 8px 32px rgba(0, 0, 0, 0.1), inset 0 1px 0 rgba(255, 255, 255, 0.1)',
        'glass-lg': '0 16px 64px rgba(0, 0, 0, 0.15), inset 0 1px 0 rgba(255, 255, 255, 0.1)',
        'glass-xl': '0 24px 96px rgba(0, 0, 0, 0.2), inset 0 1px 0 rgba(255, 255, 255, 0.1)',
        'glow': '0 0 32px rgba(14, 165, 233, 0.3)',
        'glow-warm': '0 0 32px rgba(245, 158, 11, 0.3)',
      },
      animation: {
        'float': 'float 6s ease-in-out infinite',
        'float-delayed': 'float 6s ease-in-out infinite 2s',
        'pulse-glow': 'pulse-glow 2s ease-in-out infinite',
        'shimmer': 'shimmer 2s linear infinite',
      },
      keyframes: {
        float: {
          '0%, 100%': { transform: 'translateY(0px)' },
          '50%': { transform: 'translateY(-10px)' },
        },
        'pulse-glow': {
          '0%, 100%': { boxShadow: '0 0 20px rgba(14, 165, 233, 0.3)' },
          '50%': { boxShadow: '0 0 40px rgba(14, 165, 233, 0.6)' },
        },
        shimmer: {
          '0%': { transform: 'translateX(-100%)' },
          '100%': { transform: 'translateX(100%)' },
        },
      },
    }
  }
}