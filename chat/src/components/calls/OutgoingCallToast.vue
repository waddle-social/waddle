<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneOff, Video, X } from "lucide-vue-next";
import { callToastClasses } from "./call-toast-classes";
import { $callState, $lastCallError, clearCallState, reportCallError } from "@/lib/calls/call-store";
import { clearDmCallActivity } from "@/lib/calls/dm-call-activity";
import { outboundCalls } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

// Ink in both themes (Huddle-DM board): ringing out pulses; "call ended"
// is a quiet notice.
const cls = callToastClasses;

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
  return state.value.media.video ? "Video call" : "Voice call";
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
    :class="cls.root"
    data-theme="night"
    role="dialog"
    aria-live="polite"
    aria-label="Outgoing call"
  >
    <div :class="cls.head">
      <span :class="cls.icon" data-pulse="true" aria-hidden="true">
        <component :is="state.media.video ? Video : Phone" />
      </span>
      <div :class="cls.copy">
        <div :class="cls.title">{{ peerLabel }}</div>
        <div :class="cls.status">{{ mediaLabel }} · {{ outgoingStatusLabel }}</div>
      </div>
    </div>
    <div v-if="lastError" :class="cls.alert" role="alert">
      {{ lastError }}
    </div>
    <button :class="cls.cancel" type="button" @click="cancel">
      <PhoneOff aria-hidden="true" />
      <span>Cancel</span>
    </button>
  </div>

  <div
    v-else-if="state.phase === 'ended' && endedCopy"
    :class="cls.root"
    data-theme="night"
    role="status"
    aria-live="polite"
    aria-label="Call ended"
  >
    <div :class="cls.head">
      <span :class="cls.icon" data-tone="quiet" aria-hidden="true">
        <PhoneOff />
      </span>
      <div :class="cls.copy">
        <div :class="cls.title">{{ endedCopy }}</div>
      </div>
      <button :class="cls.dismiss" type="button" aria-label="Dismiss" @click="dismissEnded">
        <X aria-hidden="true" />
      </button>
    </div>
  </div>
</template>
