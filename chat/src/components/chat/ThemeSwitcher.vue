<script setup lang="ts">
import { Monitor, Moon, Sun } from "lucide-vue-next";
import { useTheme, type ThemeMode } from "@/composables/useTheme";

const { mode, setTheme } = useTheme();

const options: ReadonlyArray<{ value: ThemeMode; label: string; icon: typeof Sun }> = [
  { value: "light", label: "Light", icon: Sun },
  { value: "system", label: "System", icon: Monitor },
  { value: "dark", label: "Dark", icon: Moon },
];
</script>

<template>
  <div class="flex items-center justify-between gap-3">
    <span class="text-[10px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/70">Theme</span>
    <div
      role="radiogroup"
      aria-label="Theme"
      class="flex items-center gap-0.5 rounded-lg border border-border bg-muted/40 p-0.5"
    >
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        role="radio"
        :aria-checked="mode === option.value"
        :aria-label="option.label"
        :title="option.label"
        class="flex h-6 w-6 items-center justify-center rounded-md transition-colors duration-150"
        :class="mode === option.value
          ? 'bg-background text-foreground shadow-sm'
          : 'text-muted-foreground hover:text-foreground'"
        @click="setTheme(option.value)"
      >
        <component :is="option.icon" class="h-3 w-3" />
      </button>
    </div>
  </div>
</template>
