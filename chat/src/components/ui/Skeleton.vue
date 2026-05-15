<script setup lang="ts">
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
    radius: "var(--radius-sm, 6px)",
    inline: false,
  },
);
</script>

<template>
  <span
    :class="['chat-skeleton', inline ? 'inline-block align-middle' : 'block']"
    :style="{ width, height, borderRadius: radius }"
    aria-hidden="true"
  />
</template>
