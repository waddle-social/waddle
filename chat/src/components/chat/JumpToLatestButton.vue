<script setup lang="ts">
import { ArrowDown, ArrowUp } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn } from "styled-system/recipes";

defineProps<{ top: boolean }>();
const emit = defineEmits<{ jump: [] }>();

const button = btn();
const floating = css({
  position: "absolute",
  left: "50%",
  transform: "translateX(-50%)",
  zIndex: "panel",
  px: "3",
  borderRadius: "pill",
  boxShadow: "elev.2",
  animation: "wd-rise {durations.base} {easings.out}",
  "& svg": { width: "16px", height: "16px" },
});
const atTop = css({ top: "4" });
const atBottom = css({ bottom: "4" });
</script>

<template>
  <button
    type="button"
    :class="[button, floating, top ? atTop : atBottom]"
    aria-label="Jump to latest message"
    title="Jump to latest"
    @click="emit('jump')"
  >
    <ArrowUp v-if="top" aria-hidden="true" />
    <ArrowDown v-else aria-hidden="true" />
    <span>Latest</span>
  </button>
</template>
