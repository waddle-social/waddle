import { ref } from 'vue';
import { defineStore } from 'pinia';

export type ThemeChoice = 'light' | 'dark' | 'system';

export const useSettingsStore = defineStore('settings', () => {
  const locale = ref('en-US');
  const notificationsEnabled = ref(true);
  const theme = ref<ThemeChoice>('light');

  function setTheme(nextTheme: ThemeChoice): void {
    theme.value = nextTheme;
  }

  return {
    locale,
    notificationsEnabled,
    theme,
    setTheme,
  };
});
