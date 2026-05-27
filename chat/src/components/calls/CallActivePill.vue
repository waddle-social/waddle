<script setup lang="ts">
import { computed } from "vue";
import { Phone, Video } from "lucide-vue-next";
import { useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";

/**
 * Live-call indicator shown in the channel header when the room owns
 * an active MUC call AND the local user is NOT currently in it.
 *
 * Replaces the previous in-button chip inside `MucCallButton` so the
 * "join" affordance now reads as a single statement: "a call is
 * happening, click to join". Muji presence proves that a call is live
 * in the room, but it does not carry a protocol-backed start time, so
 * this pill intentionally avoids rendering elapsed duration.
 *
 * Hides itself once the local user joins (the split-view surface
 * communicates the state from then on) and when the call ends.
 */
const props = defineProps<{
  /** Channel's bare MUC JID — `room@muc.host`. */
  roomJid: string;
  /** Click handler that initiates a join. Owned by the parent
   *  (typically `MucCallButton`) so all of the call-start plumbing
   *  (sender lookup, media defaults, busy guards) stays in one place. */
  onJoin: () => void;
  /** Disable the join action — set when the local user is already in
   *  a different call, or when the engine is mid-start. */
  disabled?: boolean;
}>();

const {
  hasActiveCall,
  selfInCall,
  localResourceInCall,
  participantCount,
  media,
} = useRoomHasActiveCall(
  () => props.roomJid,
);

/**
 * The pill is visible only when:
 *   - There is an active MUC call in THIS room per Muji presence.
 *   - This browser resource is not currently joined to it (otherwise
 *     the split surface is already painting the state for them).
 */
const isVisible = computed(() => hasActiveCall.value && !localResourceInCall.value);

const ariaLabel = computed(() => {
  const count = participantCount.value;
  const noun = count === 1 ? "person" : "people";
  const mediaLabel = media.value.video ? "video call" : "call";
  const device = selfInCall.value
    ? " This account is connected on another device."
    : "";
  if (props.disabled) {
    return `Live ${mediaLabel} in this channel, ${count} ${noun}.${device} Join unavailable from this device.`;
  }
  return `Live ${mediaLabel} in this channel, ${count} ${noun}.${device} Click to join from this device.`;
});

function onClick(): void {
  if (props.disabled) return;
  props.onJoin();
}
</script>

<template>
  <button
    v-if="isVisible"
    type="button"
    class="call-active-pill"
    :class="{ 'call-active-pill--disabled': disabled }"
    :disabled="disabled"
    :aria-label="ariaLabel"
    :title="ariaLabel"
    @click="onClick"
  >
    <span class="call-active-pill__dot" aria-hidden="true" />
    <Video v-if="media.video" class="call-active-pill__icon" aria-hidden="true" />
    <Phone v-else class="call-active-pill__icon" aria-hidden="true" />
    <span class="call-active-pill__count">{{ participantCount }}</span>
    <span class="call-active-pill__separator" aria-hidden="true">·</span>
    <span class="call-active-pill__state">Live</span>
  </button>
</template>

<style scoped>
.call-active-pill {
  display: inline-flex;
  align-items: center;
  gap: 0.375rem;
  padding: 0.125rem 0.625rem;
  border-radius: 9999px;
  font-size: 0.75rem;
  font-weight: 500;
  background: color-mix(in oklab, oklch(0.7 0.18 145) 12%, transparent);
  border: 1px solid color-mix(in oklab, oklch(0.7 0.18 145) 40%, transparent);
  color: oklch(0.4 0.12 145);
  cursor: pointer;
  transition: background-color 160ms ease-out, border-color 160ms ease-out,
    transform 160ms ease-out, box-shadow 160ms ease-out;
}

:global(.dark) .call-active-pill {
  color: oklch(0.85 0.13 145);
}

.call-active-pill:hover:not(:disabled) {
  background: color-mix(in oklab, oklch(0.7 0.18 145) 20%, transparent);
  border-color: color-mix(in oklab, oklch(0.7 0.18 145) 60%, transparent);
  transform: translateY(-1px);
  box-shadow: 0 4px 12px -2px color-mix(in oklab, oklch(0.7 0.18 145) 30%, transparent);
}

.call-active-pill:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px color-mix(in oklab, oklch(0.7 0.18 145) 50%, transparent);
}

.call-active-pill--disabled,
.call-active-pill:disabled {
  cursor: not-allowed;
  opacity: 0.55;
  transform: none;
  box-shadow: none;
}

.call-active-pill__dot {
  width: 0.375rem;
  height: 0.375rem;
  border-radius: 9999px;
  background: oklch(0.7 0.18 145);
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.7 0.18 145) 25%, transparent);
  animation: call-active-pill-pulse 1.6s ease-in-out infinite;
}

@media (prefers-reduced-motion: reduce) {
  .call-active-pill__dot {
    animation: none;
  }
}

@keyframes call-active-pill-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.55; transform: scale(0.85); }
}

.call-active-pill__icon {
  width: 0.75rem;
  height: 0.75rem;
}

.call-active-pill__separator {
  opacity: 0.4;
}

.call-active-pill__count,
.call-active-pill__state {
  font-variant-numeric: tabular-nums;
}
</style>
