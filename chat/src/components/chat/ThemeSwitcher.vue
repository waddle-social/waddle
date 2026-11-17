<script setup lang="ts">
import { computed } from "vue";
import { Moon, Sun, SunMoon } from "lucide-vue-next";
import { useTheme, type ThemeMode } from "@/preferences/theme";
import { css, cx } from "styled-system/css";
import { menuClasses } from "@/ui/menu-classes";

/** Account-menu row that cycles system → night → daylight. */
const { mode, setTheme } = useTheme();

const THEMES = {
  system: { icon: SunMoon, label: "System", next: "night" },
  night: { icon: Moon, label: "Night", next: "daylight" },
  daylight: { icon: Sun, label: "Daylight", next: "system" },
} as const satisfies Record<ThemeMode, { next: ThemeMode; label: string; icon: unknown }>;

const current = computed(() => THEMES[mode.value]);
const nextLabel = computed(() => THEMES[current.value.next].label);

// A plain button in a popover (no Ark highlight), so hover tints it.
const rowClass = cx(menuClasses.item, css({ _hover: { bg: "state.hover" } }));
</script>

<template>
  <button
    type="button"
    :class="rowClass"
    :aria-label="`Theme: ${current.label}. Click to switch to ${nextLabel}.`"
    :title="`Theme: ${current.label} — click for ${nextLabel}`"
    @click="setTheme(current.next)"
  >
    <component :is="current.icon" aria-hidden="true" />
    <span>Theme: {{ current.label }}</span>
  </button>
</template>
