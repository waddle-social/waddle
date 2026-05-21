<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone } from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import {
  $mucCallParticipants,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";

/**
 * Live-call indicator shown in the channel header when the room owns
 * an active MUC call AND the local user is NOT currently in it.
 *
 * Replaces the previous in-button chip inside `MucCallButton` so the
 * "join" affordance now reads as a single statement: "a call is
 * happening, click to join". The pulsing dot + running duration give
 * the chip enough motion to grab the eye without owning a full row
 * of the header.
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

const state = useStore($callState);
const participants = useStore($mucCallParticipants);
const { selfInCall, activeRoomJid } = useActiveMucCall();

const normalizedRoomJid = computed(() => normalizeMucCallRoomJid(props.roomJid));

const callInThisRoom = computed(() => {
  return (
    activeRoomJid.value !== null &&
    activeRoomJid.value === normalizedRoomJid.value
  );
});

/**
 * The pill is visible only when:
 *   - There is an active MUC call in THIS room.
 *   - The local user is not currently joined to it (otherwise the
 *     split surface is already painting the state for them).
 */
const isVisible = computed(() => callInThisRoom.value && !selfInCall.value);

const participantCount = computed<number>(() => {
  if (!callInThisRoom.value) return 0;
  return (participants.value[normalizedRoomJid.value] ?? []).length;
});

/**
 * Running call duration. Pegged to a single shared 1Hz tick so we
 * don't spawn a setInterval per pill on chat shells with many
 * channels open at once. `startedAt` does not exist on the call
 * state today — derive a stable t0 from the moment the call first
 * appeared as active in THIS room, and keep ticking from there.
 */
const callStartTimestamp = ref<number | null>(null);
const now = ref(Date.now());
let tick: ReturnType<typeof setInterval> | null = null;

function ensureStart(): void {
  if (!callInThisRoom.value) {
    callStartTimestamp.value = null;
    return;
  }
  if (callStartTimestamp.value === null) {
    callStartTimestamp.value = Date.now();
  }
}

onMounted(() => {
  ensureStart();
  tick = setInterval(() => {
    now.value = Date.now();
    ensureStart();
  }, 1000);
});

onBeforeUnmount(() => {
  if (tick) {
    clearInterval(tick);
    tick = null;
  }
});

const durationLabel = computed<string>(() => {
  if (!callStartTimestamp.value) return "00:00";
  const seconds = Math.max(0, Math.floor((now.value - callStartTimestamp.value) / 1000));
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  const mm = m.toString().padStart(2, "0");
  const ss = s.toString().padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
});

const ariaLabel = computed(() => {
  const count = participantCount.value;
  const noun = count === 1 ? "person" : "people";
  return `Live call in this channel, ${count} ${noun}, ${durationLabel.value} elapsed. Click to join.`;
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
    role="status"
    aria-live="polite"
    @click="onClick"
  >
    <span class="call-active-pill__dot" aria-hidden="true" />
    <Phone class="call-active-pill__icon" aria-hidden="true" />
    <span class="call-active-pill__count">{{ participantCount }}</span>
    <span class="call-active-pill__separator" aria-hidden="true">·</span>
    <span class="call-active-pill__duration tabular-nums">{{ durationLabel }}</span>
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
.call-active-pill__duration {
  font-variant-numeric: tabular-nums;
}
</style>
