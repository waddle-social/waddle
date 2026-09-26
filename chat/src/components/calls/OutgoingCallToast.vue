<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneOff, Video, X } from "lucide-vue-next";
import { toast as toastRecipe } from "styled-system/recipes";
import { $callState, $lastCallError, clearCallState, reportCallError } from "@/lib/calls/call-store";
import { clearDmCallActivity } from "@/lib/calls/dm-call-activity";
import { outboundCalls } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

// Styled like every other toast (opaque ink, night-coloured in daylight).
// Ringing out is live (ember border); "call ended" is a neutral notice.
const cls = toastRecipe();

const state = useStore($callState);
const lastError = useStore($lastCallError);

const peerLabel = computed(() => {
  if (state.value.phase === "outgoing") {
    const at = state.value.to.indexOf("@");
    return at > 0 ? state.value.to.slice(0, at) : state.value.to;
  }
  return "";
});

const mediaLabel = computed(() => {
  if (state.value.phase !== "outgoing") return "";
  return state.value.media.video ? "Video call" : "Audio call";
});

const outgoingStatusLabel = computed(() => {
  if (state.value.phase !== "outgoing") return "";
  return state.value.ringing ? "ringing…" : "calling…";
});

const endedCopy = computed(() => {
  if (state.value.phase !== "ended") return null;
  // The reducer stamps `reason` from the wire: `reject` /
  // `retract` come from the JMI envelope's element name, while
  // session-terminate carries the XEP-0166 reason child verbatim
  // (e.g. "success", "busy", "decline", "cancel"). Translate the
  // common values into UI strings; fall back to the raw reason.
  const r = state.value.reason;
  switch (r) {
    case null:
    case "success":
      return "Call ended";
    case "reject":
    case "decline":
      return "Call declined";
    case "retract":
    case "cancel":
      return "Call cancelled";
    case "busy":
      return "Peer is busy";
    case "timeout":
      return "No answer";
    default:
      return `Call ended (${r})`;
  }
});

function getSender() {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as Parameters<typeof outboundCalls.retract>[0] | undefined) ?? null;
}

async function cancel(): Promise<void> {
  if (state.value.phase !== "outgoing") return;
  const { to, sid } = state.value;
  const sender = getSender();
  if (sender) {
    try {
      // XEP-0353 §0.4 retract — caller aborts the propose before
      // the peer answered. Optimistically clear the slot here so
      // the toast disappears immediately; the reflected retract
      // (if any) is then a no-op against the empty state.
      await outboundCalls.retract(sender, to, sid);
    } catch (err) {
      reportCallError(err);
    }
  }
  clearDmCallActivity(to, sid);
  clearCallState();
}

function dismissEnded(): void {
  if (state.value.phase !== "ended") return;
  clearCallState();
}
</script>

<template>
  <div
    v-if="state.phase === 'outgoing'"
    :class="[cls.root, 'call-toast fixed bottom-6 right-6 z-50 w-80 max-w-[calc(100vw-2rem)] flex-col animate-slide-up']"
    role="dialog"
    aria-live="polite"
    aria-label="Outgoing call"
  >
    <div class="flex w-full items-center gap-3">
      <span
        class="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-live/15 text-live motion-safe:animate-pulse"
      >
        <component :is="state.media.video ? Video : Phone" class="w-5 h-5" />
      </span>
      <div class="min-w-0 flex-1">
        <div :class="[cls.title, 'truncate']">{{ peerLabel }}</div>
        <div :class="cls.description">{{ mediaLabel }} · {{ outgoingStatusLabel }}</div>
      </div>
    </div>
    <div
      v-if="lastError"
      class="w-full rounded-md border border-destructive/40 bg-destructive/15 px-2 py-1 type-caption text-destructive-text"
      role="alert"
    >
      {{ lastError }}
    </div>
    <div class="flex w-full items-center justify-end gap-2">
      <button
        class="call-toast__secondary inline-flex h-7 items-center gap-1.5 rounded-full border border-current/30 px-2.5 text-[12px] font-bold hover:bg-white/10"
        type="button"
        @click="cancel"
      >
        <PhoneOff class="w-3.5 h-3.5" />
        <span>Cancel</span>
      </button>
    </div>
  </div>

  <div
    v-else-if="state.phase === 'ended' && endedCopy"
    :class="[cls.root, 'call-toast fixed bottom-6 right-6 z-50 w-80 max-w-[calc(100vw-2rem)] !items-center animate-slide-up']"
    style="border-color: var(--border)"
    role="status"
    aria-live="polite"
    aria-label="Call ended"
  >
    <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-white/10 text-current">
      <PhoneOff class="w-5 h-5" />
    </span>
    <div class="min-w-0 flex-1">
      <div :class="[cls.title, 'truncate']">{{ endedCopy }}</div>
    </div>
    <button
      :class="[cls.closeTrigger, 'inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md']"
      type="button"
      aria-label="Dismiss"
      @click="dismissEnded"
    >
      <X class="h-3.5 w-3.5" aria-hidden="true" />
    </button>
  </div>
</template>
