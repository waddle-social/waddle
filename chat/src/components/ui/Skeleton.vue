<script setup lang="ts">
import { skel } from "styled-system/recipes";

withDefaults(
  defineProps<{
    width?: string;
    height?: string;
    radius?: string;
    inline?: boolean;
  }>(),
  {
    width: "100%",
    height: "0.875rem",
    radius: "var(--radii-xs)",
    inline: false,
  },
);
</script>

<template>
  <span
    :class="skel()"
    :style="{ width, height, borderRadius: radius, display: inline ? 'inline-block' : 'block', verticalAlign: inline ? 'middle' : undefined }"
    aria-hidden="true"
  />
</template>
