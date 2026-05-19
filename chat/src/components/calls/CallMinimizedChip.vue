<script setup lang="ts">
import { Maximize2, PhoneOff } from "lucide-vue-next";

/**
 * Minimal "call in progress" indicator. Shown when the user has
 * collapsed the call surface to free up screen real estate; clicking
 * the chip restores the previous mode (docked or floating).
 *
 * Renders as a small fixed-position pill in the bottom-right of the
 * viewport. The label is the channel / peer name; the trailing
 * controls give the user a one-click escape (restore + hang up).
 */
const props = defineProps<{
  label: string;
  connecting: boolean;
}>();

const emit = defineEmits<{
  restore: [];
  hangup: [];
}>();
</script>

<template>
  <aside
    class="call-minimized-chip glass-panel"
    role="dialog"
    aria-label="Active call (minimized)"
  >
    <button
      type="button"
      class="call-minimized-chip__main"
      :title="`Restore call with ${label}`"
      @click="emit('restore')"
    >
      <span class="call-minimized-chip__pulse" :class="props.connecting ? 'call-minimized-chip__pulse--connecting' : ''" />
      <span class="type-control truncate">{{ label }}</span>
      <Maximize2 class="w-3.5 h-3.5 text-muted-foreground" aria-hidden="true" />
    </button>
    <button
      type="button"
      class="chat-icon-button chat-icon-button--md call-minimized-chip__hangup"
      title="Hang up"
      aria-label="Hang up"
      @click="emit('hangup')"
    >
      <PhoneOff class="w-3.5 h-3.5" />
    </button>
  </aside>
</template>

<style scoped>
.call-minimized-chip {
  position: fixed;
  bottom: var(--space-md);
  right: var(--space-md);
  z-index: 45;
  display: flex;
  align-items: stretch;
  gap: var(--space-xs);
  padding: 0.25rem 0.5rem 0.25rem 0.25rem;
  border: 1px solid var(--border);
  border-radius: 999px;
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.3);
}

.call-minimized-chip__main {
  display: inline-flex;
  align-items: center;
  gap: var(--space-xs);
  padding: 0.375rem 0.5rem;
  border-radius: 999px;
  max-width: 18rem;
  min-width: 8rem;
  transition: background-color 0.18s ease;
}

.call-minimized-chip__main:hover {
  background: var(--muted);
}

.call-minimized-chip__pulse {
  display: inline-block;
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 999px;
  background: var(--primary);
  box-shadow: 0 0 8px var(--glow-strong);
  animation: call-chip-pulse 2.5s ease-in-out infinite;
}

.call-minimized-chip__pulse--connecting {
  background: var(--muted-foreground);
  animation: call-chip-pulse 1s ease-in-out infinite;
}

.call-minimized-chip__hangup {
  color: var(--destructive);
}

.call-minimized-chip__hangup:hover {
  background: color-mix(in oklab, var(--destructive) 12%, transparent);
}

@keyframes call-chip-pulse {
  0%, 100% { opacity: 0.85; }
  50% { opacity: 0.4; }
}

@media (prefers-reduced-motion: reduce) {
  .call-minimized-chip__pulse,
  .call-minimized-chip__pulse--connecting {
    animation: none;
  }
}
</style>
