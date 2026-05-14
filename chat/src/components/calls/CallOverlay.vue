<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Mic, MicOff, PhoneOff, Video, VideoOff } from "lucide-vue-next";
import { $callState, clearCallState } from "@/lib/calls/call-store";
import { outboundCalls } from "@/lib/calls/outbound";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { connectionStore } from "@/lib/connection-store";

const state = useStore($callState);
const { engine, remoteTracks } = useCallEngine();

const connecting = ref(false);
const micEnabled = ref(true);
const camEnabled = ref(true);

// Connect to LiveKit as soon as the call transitions to `active` with
// a populated join. Disconnect on the way out. Re-runs whenever the
// active session id changes — guards against stale subscriptions
// when the user accepts a second call after the first ended.
watch(
  () => (state.value.phase === "active" ? state.value : null),
  async (active, _prev, onCleanup) => {
    if (!active) return;
    connecting.value = true;
    try {
      await engine.connect(active.join, active.media);
      micEnabled.value = active.media.audio;
      camEnabled.value = active.media.video;
    } catch {
      // The server already verified the JWT before forwarding the
      // transport, so the most common failure here is the browser
      // denying mic/cam permissions. Hang up locally; the peer will
      // see session-terminate via the hangup path below.
      clearCallState();
    } finally {
      connecting.value = false;
    }
    onCleanup(() => {
      void engine.disconnect();
    });
  },
  { immediate: true },
);

function getSender() {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as Parameters<typeof outboundCalls.sessionTerminate>[0] | undefined) ?? null;
}

async function toggleMic(): Promise<void> {
  micEnabled.value = !micEnabled.value;
  await engine.setMicEnabled(micEnabled.value);
}

async function toggleCam(): Promise<void> {
  camEnabled.value = !camEnabled.value;
  await engine.setCameraEnabled(camEnabled.value);
}

async function hangup(): Promise<void> {
  if (state.value.phase !== "active") {
    clearCallState();
    return;
  }
  const { peer, sid } = state.value;
  const sender = getSender();
  if (sender) {
    await outboundCalls
      .sessionTerminate(sender, peer, sid, "success")
      .catch(() => undefined);
  }
  await engine.disconnect();
  clearCallState();
}

const peerLabel = computed(() => {
  if (state.value.phase !== "active") return "";
  const at = state.value.peer.indexOf("@");
  return at > 0 ? state.value.peer.slice(0, at) : state.value.peer;
});

function attachTrack(el: HTMLMediaElement | null, track: { attach: (target: HTMLMediaElement) => void; detach: () => HTMLMediaElement[] } | null): void {
  if (!el || !track) return;
  track.attach(el);
}

onBeforeUnmount(() => {
  void engine.disconnect();
});
</script>

<template>
  <div
    v-if="state.phase === 'active'"
    class="fixed inset-0 z-40 flex flex-col bg-background/95 backdrop-blur-sm"
    role="dialog"
    aria-label="Active call"
  >
    <header class="flex items-center justify-between border-b border-border px-6 py-3">
      <div class="type-chat-title">{{ peerLabel }}</div>
      <div v-if="connecting" class="type-caption text-muted-foreground">Connecting…</div>
    </header>
    <div class="grid flex-1 auto-rows-fr gap-3 p-6 sm:grid-cols-2">
      <template v-for="track in remoteTracks" :key="track.publicationSid">
        <video
          v-if="track.kind === 'video'"
          :ref="(el) => attachTrack(el as HTMLVideoElement | null, track.track as unknown as { attach: (t: HTMLMediaElement) => void; detach: () => HTMLMediaElement[] })"
          class="aspect-video w-full rounded-lg bg-muted object-cover"
          autoplay
          playsinline
        />
        <audio
          v-else
          :ref="(el) => attachTrack(el as HTMLAudioElement | null, track.track as unknown as { attach: (t: HTMLMediaElement) => void; detach: () => HTMLMediaElement[] })"
          autoplay
        />
      </template>
      <div
        v-if="remoteTracks.length === 0"
        class="col-span-full flex items-center justify-center rounded-lg border border-dashed border-border type-caption text-muted-foreground"
      >
        Waiting for the other side to share media…
      </div>
    </div>
    <footer class="flex items-center justify-center gap-3 border-t border-border px-6 py-4">
      <button
        class="chat-action-button"
        :class="micEnabled ? 'chat-action-button--secondary' : 'chat-action-button--primary'"
        type="button"
        :aria-pressed="!micEnabled"
        @click="toggleMic"
      >
        <component :is="micEnabled ? Mic : MicOff" class="w-4 h-4" />
        <span class="type-control">{{ micEnabled ? "Mute" : "Unmute" }}</span>
      </button>
      <button
        v-if="state.media.video"
        class="chat-action-button"
        :class="camEnabled ? 'chat-action-button--secondary' : 'chat-action-button--primary'"
        type="button"
        :aria-pressed="!camEnabled"
        @click="toggleCam"
      >
        <component :is="camEnabled ? Video : VideoOff" class="w-4 h-4" />
        <span class="type-control">{{ camEnabled ? "Camera off" : "Camera on" }}</span>
      </button>
      <button class="chat-action-button chat-action-button--destructive" type="button" @click="hangup">
        <PhoneOff class="w-4 h-4" />
        <span class="type-control">Hang up</span>
      </button>
    </footer>
  </div>
</template>
