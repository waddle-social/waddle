<script setup lang="ts">
import { computed } from "vue";
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
    class="call-split-handle"
    :class="{ 'call-split-handle--dragging': dragging }"
    @pointerdown="emit('press', $event)"
  >
    <GripHorizontal class="call-split-handle__grip" aria-hidden="true" />
  </div>
</template>

<style scoped>
.call-split-handle {
  position: relative;
  flex: 0 0 auto;
  height: 0.5rem;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: row-resize;
  border-block: 1px solid var(--border);
  background: color-mix(in oklab, var(--background) 92%, transparent);
  transition: background-color 160ms ease-out, border-color 160ms ease-out;
  touch-action: none;
}

.call-split-handle::before {
  /* Wider invisible hit-target so the strip is easier to grab than its
   * 8px painted height implies — UX research consistently lands on a
   * ~16px tall target for vertical resize handles. */
  content: "";
  position: absolute;
  inset: -0.25rem 0;
}

.call-split-handle__grip {
  width: 1.5rem;
  height: 0.75rem;
  color: color-mix(in oklab, var(--muted-foreground) 60%, transparent);
  pointer-events: none;
  transition: color 160ms ease-out;
}

.call-split-handle:hover,
.call-split-handle:focus-visible {
  background: color-mix(in oklab, var(--primary) 8%, var(--background));
  border-color: color-mix(in oklab, var(--primary) 30%, var(--border));
}

.call-split-handle:hover .call-split-handle__grip,
.call-split-handle:focus-visible .call-split-handle__grip {
  color: var(--primary);
}

.call-split-handle:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px color-mix(in oklab, var(--primary) 40%, transparent);
}

.call-split-handle--dragging,
.call-split-handle--dragging:hover {
  background: color-mix(in oklab, var(--primary) 12%, var(--background));
  border-color: color-mix(in oklab, var(--primary) 45%, var(--border));
}

.call-split-handle--dragging .call-split-handle__grip {
  color: var(--primary);
}
</style>
