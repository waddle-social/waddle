<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneOff, Video } from "lucide-vue-next";
import { toast as toastRecipe } from "styled-system/recipes";
import { $callState, $lastCallError, clearCallState, reportCallError } from "@/lib/calls/call-store";
import { clearDmCallActivity } from "@/lib/calls/dm-call-activity";
import { outboundCalls } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

// Styled like every other toast (opaque ink, night-coloured in daylight);
// the ember border marks it as live. It is not on the generic toaster
// because it is persistent and derived from `$callState`.
const cls = toastRecipe();

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
    :class="[cls.root, 'call-toast fixed bottom-6 right-6 z-50 w-80 max-w-[calc(100vw-2rem)] flex-col animate-slide-up']"
    role="dialog"
    aria-live="assertive"
    aria-label="Incoming call"
  >
    <div class="flex w-full items-center gap-3">
      <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-live/15 text-live">
        <component :is="state.media.video ? Video : Phone" class="w-5 h-5" />
      </span>
      <div class="min-w-0 flex-1">
        <div :class="[cls.title, 'truncate']">{{ callerLabel }}</div>
        <div :class="cls.description">{{ mediaLabel }} · {{ statusLabel }}</div>
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
        class="call-toast__secondary inline-flex h-7 items-center gap-1.5 rounded-full border border-current/30 px-2.5 text-[12px] font-bold hover:bg-white/10 disabled:opacity-50"
        type="button"
        :disabled="controlsDisabled"
        @click="decline"
      >
        <PhoneOff class="w-3.5 h-3.5" />
        <span>Decline</span>
      </button>
      <button
        :class="[cls.actionTrigger, 'inline-flex items-center gap-1.5 disabled:opacity-50']"
        type="button"
        :disabled="controlsDisabled"
        @click="accept"
      >
        <Phone class="w-3.5 h-3.5" />
        <span>{{ controlsDisabled && !declining ? "Connecting…" : "Accept" }}</span>
      </button>
    </div>
  </div>
</template>
