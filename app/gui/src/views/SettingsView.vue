<script setup lang="ts">
import { storeToRefs } from 'pinia';
import { useSettingsStore, type ThemeChoice } from '../stores/settings';

const settingsStore = useSettingsStore();
const { locale, notificationsEnabled, theme } = storeToRefs(settingsStore);

function onThemeChange(event: Event): void {
  const element = event.target as HTMLSelectElement;
  settingsStore.setTheme(element.value as ThemeChoice);
}
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Header bar -->
    <header class="flex h-12 flex-shrink-0 items-center border-b border-border px-4 shadow-sm">
      <svg class="mr-2 h-5 w-5 text-muted" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
        <path stroke-linecap="round" stroke-linejoin="round" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
      </svg>
      <h2 class="text-base font-semibold text-foreground">Settings</h2>
    </header>

    <!-- Settings content -->
    <div class="flex-1 overflow-y-auto p-6">
      <div class="mx-auto max-w-2xl space-y-6">
        <!-- Appearance section -->
        <section>
          <h3 class="mb-3 text-xs font-semibold uppercase tracking-wide text-muted">Appearance</h3>
          <div class="space-y-3">
            <label class="flex items-center justify-between rounded-lg bg-surface px-4 py-3">
              <div>
                <p class="text-sm font-medium text-foreground">Theme</p>
                <p class="text-xs text-muted">Choose your preferred colour scheme</p>
              </div>
              <select
                :value="theme"
                class="rounded bg-surface-raised px-3 py-1.5 text-sm text-foreground outline-none focus:ring-1 focus:ring-accent"
                @change="onThemeChange"
              >
                <option value="light">Light</option>
                <option value="dark">Dark</option>
                <option value="system">System</option>
              </select>
            </label>

            <label class="flex items-center justify-between rounded-lg bg-surface px-4 py-3">
              <div>
                <p class="text-sm font-medium text-foreground">Locale</p>
                <p class="text-xs text-muted">Language and region for dates and times</p>
              </div>
              <input
                v-model="locale"
                class="w-32 rounded bg-surface-raised px-3 py-1.5 text-sm text-foreground outline-none focus:ring-1 focus:ring-accent"
              />
            </label>
          </div>
        </section>

        <!-- Notifications section -->
        <section>
          <h3 class="mb-3 text-xs font-semibold uppercase tracking-wide text-muted">Notifications</h3>
          <label class="flex items-center justify-between rounded-lg bg-surface px-4 py-3">
            <div>
              <p class="text-sm font-medium text-foreground">Desktop Notifications</p>
              <p class="text-xs text-muted">Show system notifications for new messages</p>
            </div>
            <div
              class="relative h-6 w-11 cursor-pointer rounded-full transition-colors"
              :class="notificationsEnabled ? 'bg-accent' : 'bg-border'"
              @click="notificationsEnabled = !notificationsEnabled"
            >
              <div
                class="absolute left-0.5 top-0.5 h-5 w-5 rounded-full bg-white transition-transform"
                :class="notificationsEnabled ? 'translate-x-5' : 'translate-x-0'"
              ></div>
            </div>
          </label>
        </section>
      </div>
    </div>
  </div>
</template>
