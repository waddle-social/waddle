import { watch } from 'vue';

import { useSettingsStore, type ThemeChoice } from '../stores/settings';
import { useWaddle } from './useWaddle';

interface ThemeColors {
  background: string;
  foreground: string;
  surface: string;
  surfaceRaised: string;
  accent: string;
  border: string;
  success: string;
  warning: string;
  error: string;
  muted: string;
  sidebar: string;
  header: string;
  chatBg: string;
  hover: string;
  active: string;
}

type BuiltinThemeName = 'light' | 'dark' | 'high-contrast';

const BUILTIN_THEMES: Record<BuiltinThemeName, ThemeColors> = {
  light: {
    background: '#ffffff',
    foreground: '#1a1a1a',
    surface: '#f2f3f5',
    surfaceRaised: '#ebedef',
    accent: '#5865f2',
    border: '#e3e5e8',
    success: '#23a559',
    warning: '#f0b232',
    error: '#da373c',
    muted: '#6d6f78',
    sidebar: '#f2f3f5',
    header: '#ffffff',
    chatBg: '#ffffff',
    hover: 'rgba(116, 127, 141, 0.08)',
    active: 'rgba(116, 127, 141, 0.16)',
  },
  dark: {
    background: '#1e1f22',
    foreground: '#dbdee1',
    surface: '#2b2d31',
    surfaceRaised: '#313338',
    accent: '#5865f2',
    border: '#3f4147',
    success: '#23a559',
    warning: '#f0b232',
    error: '#da373c',
    muted: '#80848e',
    sidebar: '#2b2d31',
    header: '#313338',
    chatBg: '#313338',
    hover: 'rgba(79, 84, 92, 0.32)',
    active: 'rgba(79, 84, 92, 0.48)',
  },
  'high-contrast': {
    background: '#000000',
    foreground: '#ffffff',
    surface: '#1a1a1a',
    surfaceRaised: '#2a2a2a',
    accent: '#ffff00',
    border: '#ffffff',
    success: '#00ff00',
    warning: '#ffff00',
    error: '#ff0000',
    muted: '#aaaaaa',
    sidebar: '#0a0a0a',
    header: '#1a1a1a',
    chatBg: '#0a0a0a',
    hover: 'rgba(255, 255, 255, 0.08)',
    active: 'rgba(255, 255, 255, 0.16)',
  },
};

function isBuiltinThemeName(name: string): name is BuiltinThemeName {
  return Object.prototype.hasOwnProperty.call(BUILTIN_THEMES, name);
}

function resolveSystemTheme(): 'light' | 'dark' {
  if (typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches) {
    return 'dark';
  }
  return 'light';
}

function applyThemeColors(colors: ThemeColors): void {
  const root = document.documentElement;
  root.style.setProperty('--waddle-bg', colors.background);
  root.style.setProperty('--waddle-fg', colors.foreground);
  root.style.setProperty('--waddle-surface', colors.surface);
  root.style.setProperty('--waddle-surface-raised', colors.surfaceRaised);
  root.style.setProperty('--waddle-accent', colors.accent);
  root.style.setProperty('--waddle-border', colors.border);
  root.style.setProperty('--waddle-success', colors.success);
  root.style.setProperty('--waddle-warning', colors.warning);
  root.style.setProperty('--waddle-error', colors.error);
  root.style.setProperty('--waddle-muted', colors.muted);
  root.style.setProperty('--waddle-sidebar', colors.sidebar);
  root.style.setProperty('--waddle-header', colors.header);
  root.style.setProperty('--waddle-chat-bg', colors.chatBg);
  root.style.setProperty('--waddle-hover', colors.hover);
  root.style.setProperty('--waddle-active', colors.active);
}

function applyThemeChoice(choice: ThemeChoice): void {
  const resolved = choice === 'system' ? resolveSystemTheme() : choice;
  const colors = BUILTIN_THEMES[resolved];
  if (colors) {
    applyThemeColors(colors);
  }
}

export function applyPluginColors(pluginId: string, tokens: Record<string, string>): void {
  const root = document.documentElement;
  for (const [token, value] of Object.entries(tokens)) {
    root.style.setProperty(`--waddle-plugin-${pluginId}-${token}`, value);
  }
}

export function useTheme(): void {
  const settings = useSettingsStore();
  const waddle = useWaddle();

  applyThemeChoice(settings.theme);

  watch(() => settings.theme, applyThemeChoice);

  if (typeof window !== 'undefined') {
    const mql = window.matchMedia('(prefers-color-scheme: dark)');
    mql.addEventListener('change', () => {
      if (settings.theme === 'system') {
        applyThemeChoice('system');
      }
    });
  }

  waddle.listen<{ name: string }>('ui.theme.changed', (event) => {
    const name = event.payload.name;
    if (isBuiltinThemeName(name)) {
      applyThemeColors(BUILTIN_THEMES[name]);
    }
  });
}
