<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneOff, Video } from "lucide-vue-next";
import { $callState, $lastCallError, clearCallState, reportCallError } from "@/lib/calls/call-store";
import { clearDmCallActivity } from "@/lib/calls/dm-call-activity";
import { outboundCalls } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

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
    class="fixed bottom-6 right-6 z-50 w-80 rounded-xl border border-border bg-popover p-4 shadow-2xl glass-surface"
    role="dialog"
    aria-live="polite"
    aria-label="Outgoing call"
  >
    <div class="flex items-center gap-3">
      <span
        class="flex h-10 w-10 items-center justify-center rounded-full bg-primary/10 text-primary motion-safe:animate-pulse"
      >
        <component :is="state.media.video ? Video : Phone" class="w-5 h-5" />
      </span>
      <div class="min-w-0 flex-1">
        <div class="type-chat-title truncate">{{ peerLabel }}</div>
        <div class="type-caption text-muted-foreground">{{ mediaLabel }} · {{ outgoingStatusLabel }}</div>
      </div>
    </div>
    <div
      v-if="lastError"
      class="mt-2 rounded-md border border-destructive/30 bg-destructive/10 px-2 py-1 type-caption text-destructive"
      role="alert"
    >
      {{ lastError }}
    </div>
    <div class="mt-3 flex items-center justify-end gap-2">
      <button
        class="chat-action-button chat-action-button--secondary"
        type="button"
        @click="cancel"
      >
        <PhoneOff class="w-4 h-4" />
        <span class="type-control">Cancel</span>
      </button>
    </div>
  </div>

  <div
    v-else-if="state.phase === 'ended' && endedCopy"
    class="fixed bottom-6 right-6 z-50 w-80 rounded-xl border border-border bg-popover p-4 shadow-2xl glass-surface"
    role="status"
    aria-live="polite"
    aria-label="Call ended"
  >
    <div class="flex items-center gap-3">
      <span class="flex h-10 w-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
        <PhoneOff class="w-5 h-5" />
      </span>
      <div class="min-w-0 flex-1">
        <div class="type-chat-title truncate">{{ endedCopy }}</div>
      </div>
      <button
        class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
        type="button"
        aria-label="Dismiss"
        @click="dismissEnded"
      >
        <span aria-hidden="true">×</span>
      </button>
    </div>
  </div>
</template>
