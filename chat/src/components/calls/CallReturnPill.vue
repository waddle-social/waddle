<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import { PhoneCall } from "lucide-vue-next";
import { $activeCallStartedAt } from "@/lib/calls/call-store";
import { $persistedCallStub } from "@/lib/calls/call-persistence";
import {
  $mucCallParticipants,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";
import { channelIdForRoomJid } from "@/lib/channel-room";
import type { ChannelSummary } from "@/lib/chat-types";

/**
 * Global "return to your call" pill rendered in the app shell.
 *
 * Two visibility modes:
 *
 * 1. **live** — the local user is in an active MUC group call AND
 *    the currently-viewed room is not the call's room. The pill
 *    shows running duration sourced from `$activeCallStartedAt`
 *    and a "Return" affordance. Clicking it navigates to the
 *    room.
 *
 * 2. **rejoin** — the in-memory call state is gone (typically a
 *    hard refresh tore down the WebSocket + WASM client) but a
 *    `$persistedCallStub` survives in sessionStorage AND that
 *    room's XEP-0272 Muji participant list still contains other
 *    active nicks (the call hasn't ended yet). The pill shows
 *    "Rejoin" with no duration — the local user's stay was
 *    interrupted, so the previous duration is meaningless. Click
 *    navigates to the room where `MucCallButton` / `CallActivePill`
 *    drive the actual rejoin.
 *
 * Both modes are hidden when the user is already viewing the
 * room — the per-channel surfaces (`CallSplitContainer`,
 * `CallActivePill`) own the in-room story.
 */
const props = defineProps<{
  /** Bare room JID the user is currently viewing, or `null` when
   *  the active surface isn't a channel (dashboard, settings,
   *  threads, DMs, etc.). The pill renders for any non-matching
   *  case, including the null case. */
  viewedRoomJid: string | null;
  /** Channel list the controller already exposes. Used to resolve
   *  the active call's room JID back to a local channel id so the
   *  click handler can navigate via `selectChannel`. */
  channels: readonly Pick<ChannelSummary, "id" | "jid" | "name">[];
  /** Navigate to a channel by id. Wired to the controller's
   *  `selectChannel` so the click reuses the existing room-load
   *  + message-fetch path. */
  onNavigate: (channelId: string) => void;
}>();

const startedAt = useStore($activeCallStartedAt);
const stub = useStore($persistedCallStub);
const participants = useStore($mucCallParticipants);
const { selfInCall, activeRoomJid } = useActiveMucCall();

const liveRoomJid = computed<string | null>(() => {
  if (!selfInCall.value) return null;
  return activeRoomJid.value;
});

const rejoinRoomJid = computed<string | null>(() => {
  if (selfInCall.value) return null;
  const persisted = stub.value;
  if (!persisted) return null;
  const normalized = normalizeMucCallRoomJid(persisted.roomJid) || persisted.roomJid;
  const nicks = participants.value[normalized] ?? [];
  if (nicks.length === 0) return null;
  return normalized;
});

const targetRoomJid = computed<string | null>(() => {
  return liveRoomJid.value ?? rejoinRoomJid.value;
});

const mode = computed<"live" | "rejoin" | "hidden">(() => {
  if (!targetRoomJid.value) return "hidden";
  const viewed = props.viewedRoomJid
    ? normalizeMucCallRoomJid(props.viewedRoomJid) || props.viewedRoomJid
    : null;
  if (viewed === targetRoomJid.value) return "hidden";
  return liveRoomJid.value ? "live" : "rejoin";
});

const targetChannel = computed(() => {
  const room = targetRoomJid.value;
  if (!room) return null;
  const channelId = channelIdForRoomJid(props.channels, room);
  if (!channelId) return null;
  const channel = props.channels.find((c) => c.id === channelId);
  return { channelId, name: channel?.name ?? channelId };
});

const participantCount = computed<number>(() => {
  const room = targetRoomJid.value;
  if (!room) return 0;
  return (participants.value[room] ?? []).length;
});

const now = ref(Date.now());
let tick: ReturnType<typeof setInterval> | null = null;

onMounted(() => {
  tick = setInterval(() => {
    now.value = Date.now();
  }, 1000);
});

onBeforeUnmount(() => {
  if (tick) {
    clearInterval(tick);
    tick = null;
  }
});

const durationLabel = computed<string | null>(() => {
  if (mode.value !== "live") return null;
  const start = startedAt.value;
  if (start === null) return null;
  const seconds = Math.max(0, Math.floor((now.value - start) / 1000));
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  const mm = m.toString().padStart(2, "0");
  const ss = s.toString().padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
});

const verb = computed<string>(() => (mode.value === "rejoin" ? "Rejoin" : "Return to"));

const channelLabel = computed<string>(() => {
  const target = targetChannel.value;
  if (target) return `#${target.name}`;
  const room = targetRoomJid.value;
  if (!room) return "";
  const local = room.split("@")[0];
  return local ? `#${local}` : room;
});

const ariaLabel = computed<string>(() => {
  if (mode.value === "live") {
    const count = participantCount.value;
    const noun = count === 1 ? "person" : "people";
    return `You are in a call in ${channelLabel.value} with ${count} ${noun}, ${durationLabel.value ?? "0:00"} elapsed. Click to return.`;
  }
  const count = participantCount.value;
  const noun = count === 1 ? "person" : "people";
  return `A call you were in is still ongoing in ${channelLabel.value} with ${count} ${noun}. Click to rejoin.`;
});

function onClick(): void {
  const target = targetChannel.value;
  if (!target) return;
  props.onNavigate(target.channelId);
}
</script>

<template>
  <button
    v-if="mode !== 'hidden' && targetChannel"
    type="button"
    class="call-return-pill"
    :class="[`call-return-pill--${mode}`]"
    :aria-label="ariaLabel"
    :title="ariaLabel"
    role="status"
    aria-live="polite"
    @click="onClick"
  >
    <span class="call-return-pill__dot" aria-hidden="true" />
    <PhoneCall class="call-return-pill__icon" aria-hidden="true" />
    <span class="call-return-pill__verb">{{ verb }}</span>
    <span class="call-return-pill__room">{{ channelLabel }}</span>
    <template v-if="durationLabel">
      <span class="call-return-pill__separator" aria-hidden="true">·</span>
      <span class="call-return-pill__duration tabular-nums">{{ durationLabel }}</span>
    </template>
  </button>
</template>

<style scoped>
.call-return-pill {
  position: fixed;
  top: calc(env(safe-area-inset-top, 0px) + 0.75rem);
  left: 50%;
  transform: translateX(-50%);
  z-index: 60;
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.375rem 0.875rem;
  border-radius: 9999px;
  font-size: 0.8125rem;
  font-weight: 600;
  color: white;
  cursor: pointer;
  transition: transform 160ms ease-out, box-shadow 160ms ease-out,
    background-color 160ms ease-out;
  box-shadow: 0 6px 20px -4px color-mix(in oklab, oklch(0.6 0.2 25) 45%, transparent),
    0 2px 6px -1px rgba(0, 0, 0, 0.15);
  backdrop-filter: blur(6px);
}

.call-return-pill--live {
  background: linear-gradient(
    180deg,
    oklch(0.65 0.18 25) 0%,
    oklch(0.58 0.2 25) 100%
  );
  border: 1px solid color-mix(in oklab, oklch(0.7 0.2 25) 70%, transparent);
}

.call-return-pill--rejoin {
  background: linear-gradient(
    180deg,
    oklch(0.7 0.16 75) 0%,
    oklch(0.62 0.18 65) 100%
  );
  border: 1px solid color-mix(in oklab, oklch(0.72 0.18 70) 70%, transparent);
}

.call-return-pill:hover {
  transform: translateX(-50%) translateY(-1px);
  box-shadow: 0 10px 28px -4px color-mix(in oklab, oklch(0.6 0.2 25) 55%, transparent),
    0 4px 10px -1px rgba(0, 0, 0, 0.2);
}

.call-return-pill:focus-visible {
  outline: none;
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.75 0.18 25) 55%, transparent);
}

.call-return-pill__dot {
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 9999px;
  background: white;
  box-shadow: 0 0 0 3px color-mix(in oklab, white 30%, transparent);
  animation: call-return-pill-pulse 1.6s ease-in-out infinite;
}

@media (prefers-reduced-motion: reduce) {
  .call-return-pill__dot {
    animation: none;
  }
}

@keyframes call-return-pill-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.55; transform: scale(0.8); }
}

.call-return-pill__icon {
  width: 0.875rem;
  height: 0.875rem;
}

.call-return-pill__separator {
  opacity: 0.55;
}

.call-return-pill__room {
  font-weight: 700;
  letter-spacing: 0.01em;
}

.call-return-pill__duration {
  font-variant-numeric: tabular-nums;
  opacity: 0.92;
}
</style>
