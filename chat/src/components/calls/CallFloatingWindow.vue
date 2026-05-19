<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { GripHorizontal } from "lucide-vue-next";
import { $callFloatingPosition } from "@/lib/calls/ui-mode";

/**
 * A frameless draggable floating window. Hosts whatever the caller
 * slots in (`<slot name="header" />` + default slot for body); the
 * window itself owns position + drag interaction and persists the
 * top-left corner through `$callFloatingPosition`.
 *
 * Constraints:
 * - Mouse + touch supported via Pointer Events.
 * - The window clamps to the viewport on resize / drag-end so a
 *   user can't accidentally park it off-screen.
 * - Initial position falls back to top-right if no saved position.
 *
 * Drag affordance: the header is the drag handle (anywhere within
 * it). The body content (the call grid) intentionally does NOT
 * initiate drags so interactive elements like the controls stay
 * usable without a "stop propagation" dance.
 */
const persisted = useStore($callFloatingPosition);
const dragging = ref(false);
const width = ref(360);
const height = ref(480);

const position = ref<{ x: number; y: number }>(initialPosition());

function initialPosition(): { x: number; y: number } {
  if (persisted.value) return clampToViewport(persisted.value);
  if (typeof window === "undefined") return { x: 24, y: 96 };
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  return clampToViewport({ x: vw - width.value - 24, y: Math.min(96, vh - height.value - 24) });
}

function clampToViewport(p: { x: number; y: number }): { x: number; y: number } {
  if (typeof window === "undefined") return p;
  const maxX = Math.max(0, window.innerWidth - width.value);
  const maxY = Math.max(0, window.innerHeight - height.value);
  return {
    x: Math.min(Math.max(0, p.x), maxX),
    y: Math.min(Math.max(0, p.y), maxY),
  };
}

const style = computed(() => ({
  transform: `translate(${position.value.x}px, ${position.value.y}px)`,
  width: `${width.value}px`,
  height: `${height.value}px`,
}));

let dragOffset: { x: number; y: number } | null = null;

function onPointerDown(e: PointerEvent): void {
  if (e.button !== 0) return;
  dragging.value = true;
  dragOffset = {
    x: e.clientX - position.value.x,
    y: e.clientY - position.value.y,
  };
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
}

function onPointerMove(e: PointerEvent): void {
  if (!dragging.value || !dragOffset) return;
  position.value = clampToViewport({
    x: e.clientX - dragOffset.x,
    y: e.clientY - dragOffset.y,
  });
}

function onPointerUp(e: PointerEvent): void {
  if (!dragging.value) return;
  dragging.value = false;
  dragOffset = null;
  $callFloatingPosition.set(position.value);
  (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
}

function onViewportResize(): void {
  position.value = clampToViewport(position.value);
}

if (typeof window !== "undefined") {
  window.addEventListener("resize", onViewportResize);
}

onBeforeUnmount(() => {
  if (typeof window !== "undefined") {
    window.removeEventListener("resize", onViewportResize);
  }
});

watch(persisted, (next) => {
  if (next) position.value = clampToViewport(next);
});
</script>

<template>
  <aside
    class="call-floating-window glass-panel"
    :style="style"
    role="dialog"
    aria-label="Floating call window"
  >
    <header
      class="call-floating-window__header"
      :class="dragging ? 'call-floating-window__header--dragging' : ''"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
    >
      <GripHorizontal class="w-4 h-4 text-muted-foreground" aria-hidden="true" />
      <slot name="header" />
    </header>
    <div class="call-floating-window__body">
      <slot />
    </div>
    <footer class="call-floating-window__footer">
      <slot name="footer" />
    </footer>
  </aside>
</template>

<style scoped>
.call-floating-window {
  position: fixed;
  top: 0;
  left: 0;
  display: flex;
  flex-direction: column;
  border-radius: var(--radius-lg);
  border: 1px solid var(--border);
  overflow: hidden;
  z-index: 45;
  box-shadow: 0 18px 48px rgba(0, 0, 0, 0.32);
}

.call-floating-window__header {
  display: flex;
  align-items: center;
  gap: var(--space-xs);
  padding: 0.5rem 0.75rem;
  border-bottom: 1px solid var(--border);
  background: color-mix(in oklab, var(--surface) 80%, transparent);
  cursor: grab;
  user-select: none;
  touch-action: none;
}

.call-floating-window__header--dragging {
  cursor: grabbing;
}

.call-floating-window__body {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.call-floating-window__footer {
  border-top: 1px solid var(--border);
  padding: 0.5rem 0.75rem;
}
</style>
