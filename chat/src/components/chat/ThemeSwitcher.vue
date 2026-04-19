<script setup lang="ts">
import { computed } from "vue";
import { Monitor, Moon, Sun } from "lucide-vue-next";
import { useTheme, type ThemeMode } from "@/composables/useTheme";

const { mode, setTheme } = useTheme();

const CYCLE: ReadonlyArray<ThemeMode> = ["light", "system", "dark"];

const current = computed(() => {
  switch (mode.value) {
    case "light":
      return { icon: Sun, label: "Light" };
    case "dark":
      return { icon: Moon, label: "Dark" };
    default:
      return { icon: Monitor, label: "System" };
  }
});

const nextLabel = computed(() => {
  const idx = CYCLE.indexOf(mode.value);
  const next = CYCLE[(idx + 1) % CYCLE.length];
  return next.charAt(0).toUpperCase() + next.slice(1);
});

function cycle() {
  const idx = CYCLE.indexOf(mode.value);
  const next = CYCLE[(idx + 1) % CYCLE.length];
  setTheme(next);
}
</script>

<template>
  <button
    type="button"
    class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-lg text-sidebar-muted transition-all duration-200 hover:bg-sidebar-accent hover:text-primary"
    :aria-label="`Theme: ${current.label}. Click to switch to ${nextLabel}.`"
    :title="`Theme: ${current.label} — click for ${nextLabel}`"
    @click="cycle"
  >
    <component :is="current.icon" class="h-3.5 w-3.5" />
  </button>
</template>
