<script setup lang="ts">
import { computed } from "vue";
import { css } from "styled-system/css";
import { GripHorizontal } from "lucide-vue-next";

/**
 * Visual grip + pointer target for the call/chat splitter.
 *
 * Stateless: the parent owns the resize composable
 * (`useSplitResize`) and passes the press handler in. Keeping the
 * handle dumb lets the same component sit between any pair of flex
 * children in the future (panel splitter, drawer divider, etc.)
 * without ferrying call-specific state through.
 */
const props = defineProps<{
  /** Visual feedback only — when true, the handle paints itself in
   *  the primary tint and the cursor hint stays `row-resize` even
   *  after the pointer leaves the strip. */
  dragging: boolean;
  /** Current split percentage [25, 75]. Used to populate
   *  `aria-valuenow` so screen readers announce the position. */
  percent: number;
}>();

const emit = defineEmits<{
  press: [event: PointerEvent];
}>();

const valueNow = computed(() => Math.round(props.percent));

// A thin seam between the dark stage and the chat below; a wider
// invisible hit target (::before) makes it easy to grab.
const rootClass = css({
  position: "relative",
  flex: "0 0 auto",
  height: "10px",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  cursor: "row-resize",
  bg: "bg",
  borderBlock: "1px solid {colors.border.soft}",
  color: "fg.soft",
  touchAction: "none",
  transition: "background-color {durations.fast} {easings.out}, color {durations.fast} {easings.out}",
  _before: { content: '""', position: "absolute", inset: "-6px 0" },
  "& svg": { width: "24px", height: "12px", pointerEvents: "none" },
  _hover: { bg: "state.hover", color: "fg" },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "-2px", color: "fg" },
});
const draggingClass = css({ bg: "action.subtle", color: "action.text", _hover: { bg: "action.subtle", color: "action.text" } });
</script>

<template>
  <div
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize the call and chat regions"
    :aria-valuenow="valueNow"
    aria-valuemin="25"
    aria-valuemax="75"
    tabindex="0"
    :class="[rootClass, dragging && draggingClass]"
    @pointerdown="emit('press', $event)"
  >
    <GripHorizontal aria-hidden="true" />
  </div>
</template>
