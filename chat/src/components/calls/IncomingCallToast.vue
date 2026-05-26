<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneOff, Video } from "lucide-vue-next";
import { $callState, $lastCallError, clearCallState, reportCallError } from "@/lib/calls/call-store";
import { clearDmCallActivity } from "@/lib/calls/dm-call-activity";
import { outboundCalls } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

const state = useStore($callState);
const lastError = useStore($lastCallError);

// Local in-flight refs so a double-click can't fire two wire sends
// and the user sees "Connecting…" feedback between the JMI proceed
// and the inbound session-initiate that flips us to active.
const accepting = ref(false);
const declining = ref(false);

// Reset the in-flight flags whenever the phase moves off `incoming`
// (either we accepted and the call became active, or it ended).
watch(
  () => state.value.phase,
  (phase) => {
    if (phase !== "incoming") {
      accepting.value = false;
      declining.value = false;
    }
  },
);

const callerLabel = computed(() => {
  if (state.value.phase !== "incoming") return "";
  const at = state.value.from.indexOf("@");
  return at > 0 ? state.value.from.slice(0, at) : state.value.from;
});

const mediaLabel = computed(() => {
  if (state.value.phase !== "incoming") return "";
  return state.value.media.video ? "Video call" : "Audio call";
});

const statusLabel = computed(() => {
  if (accepting.value || (state.value.phase === "incoming" && state.value.accepting)) return "Connecting…";
  if (declining.value) return "Declining…";
  return "ringing…";
});

const controlsDisabled = computed(() =>
  accepting.value ||
  declining.value ||
  (state.value.phase === "incoming" && state.value.accepting === true),
);

function getSender() {
  // BrowserXmppClient stores the wasm client as `xmpp`; cast through unknown
  // because the field is private on the class and we're treating it as an
  // implementation detail of this UI surface.
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as Parameters<typeof outboundCalls.proceed>[0] | undefined) ?? null;
}

async function accept(): Promise<void> {
  if (state.value.phase !== "incoming") return;
  if (controlsDisabled.value) return;
  const { from, sid } = state.value;
  const sender = getSender();
  if (!sender) return;
  accepting.value = true;
  try {
    // Send <proceed/> back to the caller's full JID; they respond
    // with a Jingle session-initiate which transitions us to active.
    await outboundCalls.proceed(sender, from, sid);
  } catch (err) {
    accepting.value = false;
    reportCallError(err);
  }
}

async function decline(): Promise<void> {
  if (state.value.phase !== "incoming") return;
  if (controlsDisabled.value) return;
  const { from, sid } = state.value;
  const sender = getSender();
  declining.value = true;
  if (sender) {
    try {
      await outboundCalls.reject(sender, from, sid);
    } catch (err) {
      reportCallError(err);
    }
  }
  clearDmCallActivity(from, sid);
  clearCallState();
}
</script>

<template>
  <div
    v-if="state.phase === 'incoming'"
    class="fixed bottom-6 right-6 z-50 w-80 rounded-xl border border-border bg-popover p-4 shadow-2xl glass-surface"
    role="dialog"
    aria-live="assertive"
    aria-label="Incoming call"
  >
    <div class="flex items-center gap-3">
      <span class="flex h-10 w-10 items-center justify-center rounded-full bg-primary/10 text-primary">
        <component :is="state.media.video ? Video : Phone" class="w-5 h-5" />
      </span>
      <div class="min-w-0 flex-1">
        <div class="type-chat-title truncate">{{ callerLabel }}</div>
        <div class="type-caption text-muted-foreground">{{ mediaLabel }} · {{ statusLabel }}</div>
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
        :disabled="controlsDisabled"
        @click="decline"
      >
        <PhoneOff class="w-4 h-4" />
        <span class="type-control">Decline</span>
      </button>
      <button
        class="chat-action-button chat-action-button--primary"
        type="button"
        :disabled="controlsDisabled"
        @click="accept"
      >
        <Phone class="w-4 h-4" />
        <span class="type-control">{{ controlsDisabled && !declining ? "Connecting…" : "Accept" }}</span>
      </button>
    </div>
  </div>
</template>
