<script setup lang="ts">
import { computed } from "vue";
import { Moon, Sun } from "lucide-vue-next";
import { useTheme, type ThemeMode } from "@/preferences/theme";
import { css, cx } from "styled-system/css";
import { menuClasses } from "@/ui/menu-classes";

/** Account-menu row that flips night ⇄ daylight. */
const { mode, setTheme } = useTheme();

const CYCLE: ReadonlyArray<ThemeMode> = ["night", "daylight"];

const current = computed(() => {
  return mode.value === "daylight"
    ? { icon: Sun, label: "Daylight" }
    : { icon: Moon, label: "Night" };
});

const nextLabel = computed(() => {
  const idx = CYCLE.indexOf(mode.value);
  const next = CYCLE[(idx + 1) % CYCLE.length];
  return next === "daylight" ? "Daylight" : "Night";
});

// A plain button in a popover (no Ark highlight), so hover tints it.
const rowClass = cx(menuClasses.item, css({ _hover: { bg: "state.hover" } }));

function cycle() {
  const idx = CYCLE.indexOf(mode.value);
  const next = CYCLE[(idx + 1) % CYCLE.length];
  setTheme(next!);
}
</script>

<template>
  <button
    type="button"
    :class="rowClass"
    :aria-label="`Theme: ${current.label}. Click to switch to ${nextLabel}.`"
    :title="`Theme: ${current.label} — click for ${nextLabel}`"
    @click="cycle"
  >
    <component :is="current.icon" aria-hidden="true" />
    <span>Theme: {{ current.label }}</span>
  </button>
</template>
