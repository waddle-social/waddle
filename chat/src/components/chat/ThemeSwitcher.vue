<script setup lang="ts">
import { computed } from "vue";
import { Moon, Sun } from "lucide-vue-next";
import { useTheme, type ThemeMode } from "@/preferences/theme";

const { mode, setTheme } = useTheme();

const CYCLE: ReadonlyArray<ThemeMode> = ["dark", "light"];

const current = computed(() => {
  return mode.value === "light"
    ? { icon: Sun, label: "Daylight" }
    : { icon: Moon, label: "Night" };
});

const nextLabel = computed(() => {
  const idx = CYCLE.indexOf(mode.value);
  const next = CYCLE[(idx + 1) % CYCLE.length];
  return next === "light" ? "Daylight" : "Night";
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
    class="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-lg text-sidebar-muted transition-all duration-200 hover:bg-sidebar-accent hover:text-primary"
    :aria-label="`Theme: ${current.label}. Click to switch to ${nextLabel}.`"
    :title="`Theme: ${current.label} — click for ${nextLabel}`"
    @click="cycle"
  >
    <component :is="current.icon" class="h-3.5 w-3.5" />
  </button>
</template>
