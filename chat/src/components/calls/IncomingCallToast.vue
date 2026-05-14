<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneOff, Video } from "lucide-vue-next";
import { $callState, clearCallState } from "@/lib/calls/call-store";
import { outboundCalls } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

const state = useStore($callState);

const callerLabel = computed(() => {
  if (state.value.phase !== "incoming") return "";
  const at = state.value.from.indexOf("@");
  return at > 0 ? state.value.from.slice(0, at) : state.value.from;
});

const mediaLabel = computed(() => {
  if (state.value.phase !== "incoming") return "";
  return state.value.media.video ? "Video call" : "Audio call";
});

function getSender() {
  // BrowserXmppClient stores the wasm client as `xmpp`; we cast through unknown
  // because the field is private on the class and we're treating it as an
  // implementation detail of this UI surface.
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as Parameters<typeof outboundCalls.proceed>[0] | undefined) ?? null;
}

async function accept(): Promise<void> {
  if (state.value.phase !== "incoming") return;
  const { from, sid } = state.value;
  const sender = getSender();
  if (!sender) return;
  // Send <proceed/> back to the caller's bare JID; they will respond
  // with a Jingle session-initiate which transitions us to active.
  await outboundCalls.proceed(sender, from, sid).catch(() => undefined);
}

async function decline(): Promise<void> {
  if (state.value.phase !== "incoming") return;
  const { from, sid } = state.value;
  const sender = getSender();
  if (sender) {
    await outboundCalls.reject(sender, from, sid).catch(() => undefined);
  }
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
        <div class="type-caption text-muted-foreground">{{ mediaLabel }} · ringing…</div>
      </div>
    </div>
    <div class="mt-3 flex items-center justify-end gap-2">
      <button
        class="chat-action-button chat-action-button--secondary"
        type="button"
        @click="decline"
      >
        <PhoneOff class="w-4 h-4" />
        <span class="type-control">Decline</span>
      </button>
      <button
        class="chat-action-button chat-action-button--primary"
        type="button"
        @click="accept"
      >
        <Phone class="w-4 h-4" />
        <span class="type-control">Accept</span>
      </button>
    </div>
  </div>
</template>
