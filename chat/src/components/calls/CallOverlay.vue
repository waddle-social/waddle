<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { Mic, MicOff, PhoneOff, Video, VideoOff } from "lucide-vue-next";
import {
  $callState,
  $lastCallError,
  clearCallState,
  reportCallError,
  sendMucCallLeave,
  tearDownActiveCall,
} from "@/lib/calls/call-store";
import { outboundCalls } from "@/lib/calls/outbound";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { connectionStore } from "@/lib/connection-store";
import AppAvatar from "@/components/ui/AppAvatar.vue";

const state = useStore($callState);
const lastError = useStore($lastCallError);
const { engine, remoteTracks, localTracks } = useCallEngine();

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
  try {
    await engine.setMicEnabled(micEnabled.value);
  } catch (err) {
    micEnabled.value = !micEnabled.value;
    reportCallError(err);
  }
}

async function toggleCam(): Promise<void> {
  // First click on an audio-started call publishes the camera for
  // the first time; LiveKit's `setCameraEnabled(true)` handles both
  // the initial publish AND subsequent unmute, so we don't need a
  // separate "promote audio call to video" path.
  camEnabled.value = !camEnabled.value;
  try {
    await engine.setCameraEnabled(camEnabled.value);
  } catch (err) {
    // Camera permission denied / no device — roll back the toggle so
    // the button reflects the actual published state.
    camEnabled.value = !camEnabled.value;
    reportCallError(err);
  }
}

async function hangup(): Promise<void> {
  if (state.value.phase !== "active") {
    clearCallState();
    return;
  }
  const active = state.value;
  const sender = getSender();
  if (sender) {
    try {
      if (active.kind === "dm") {
        await outboundCalls.sessionTerminate(sender, active.peer, active.sid, "success");
      } else {
        await sendMucCallLeave(sender as unknown as Parameters<typeof sendMucCallLeave>[0], active.peer);
      }
    } catch (err) {
      reportCallError(err);
    }
  }
  await engine.disconnect();
  clearCallState();
}

const peerLabel = computed(() => {
  if (state.value.phase !== "active") return "";
  const at = state.value.peer.indexOf("@");
  const localpart = at > 0 ? state.value.peer.slice(0, at) : state.value.peer;
  // MUC group call: prefix the room's localpart with `#` so the
  // header reads as a channel reference, not a DM peer name.
  return state.value.kind === "muc" ? `#${localpart}` : localpart;
});

/**
 * Pretty display name for a LiveKit identity. The SFU mints the
 * identity from the XMPP full JID (e.g. `alice@waddle.social/web-…`),
 * so the localpart is the meaningful label. Falls back to the raw
 * identity if it doesn't look like a JID.
 */
function displayNameForIdentity(identity: string): string {
  const at = identity.indexOf("@");
  if (at <= 0) return identity;
  return identity.slice(0, at);
}

/**
 * Tiles to render in the call dock. One per (identity, kind=video)
 * pair (so each participant gets exactly one video tile if they have
 * one), plus one avatar tile per audio-only participant. The local
 * participant always gets its own tile at the front — even before
 * they publish a camera — so the user knows the dock is alive and
 * pointed at them.
 */
type Tile = {
  key: string;
  identity: string;
  label: string;
  isSelf: boolean;
  /** Present when this tile has a video track to attach. */
  videoTrack: { attach: (el: HTMLMediaElement) => void; detach: () => HTMLMediaElement[] } | null;
  /** Off-screen audio sink — every audio track needs an `<audio>` to
   *  actually play, but the visible card is the per-identity tile. */
  audioTrack: { attach: (el: HTMLMediaElement) => void; detach: () => HTMLMediaElement[] } | null;
};

const tiles = computed<Tile[]>(() => {
  const byIdentity = new Map<string, Tile>();
  // Local participant first so it always anchors the dock — even
  // pre-publish the user sees an "awaiting camera" placeholder for
  // their own slot.
  const selfId = engine.localIdentity ?? "you";
  byIdentity.set(selfId, {
    key: `self:${selfId}`,
    identity: selfId,
    label: "You",
    isSelf: true,
    videoTrack: null,
    audioTrack: null,
  });
  for (const local of localTracks.value) {
    const tile = byIdentity.get(local.participantIdentity)
      ?? {
        key: `self:${local.participantIdentity}`,
        identity: local.participantIdentity,
        label: "You",
        isSelf: true,
        videoTrack: null,
        audioTrack: null,
      };
    if (local.kind === "video") tile.videoTrack = local.track;
    // The local mic playback is intentionally suppressed (no echo),
    // so we DON'T attach `local.audioTrack` even when published.
    byIdentity.set(local.participantIdentity, tile);
  }
  for (const remote of remoteTracks.value) {
    const tile = byIdentity.get(remote.participantIdentity)
      ?? {
        key: `remote:${remote.participantIdentity}`,
        identity: remote.participantIdentity,
        label: displayNameForIdentity(remote.participantIdentity),
        isSelf: false,
        videoTrack: null,
        audioTrack: null,
      };
    if (remote.kind === "video") tile.videoTrack = remote.track;
    if (remote.kind === "audio") tile.audioTrack = remote.track;
    byIdentity.set(remote.participantIdentity, tile);
  }
  return Array.from(byIdentity.values());
});

const remoteParticipantCount = computed(() => {
  const ids = new Set<string>();
  for (const t of remoteTracks.value) ids.add(t.participantIdentity);
  return ids.size;
});

function attachTrack(
  el: HTMLMediaElement | null,
  track: { attach: (target: HTMLMediaElement) => void } | null,
): void {
  if (!el || !track) return;
  track.attach(el);
}

onBeforeUnmount(() => {
  // Tab close mid-call: best-effort session-terminate (reason
  // "gone" per XEP-0166 §7.4 for the no-longer-reachable case) so
  // the peer's overlay closes immediately instead of waiting for
  // the LiveKit room to drop them.
  void tearDownActiveCall(getSender(), "gone");
  void engine.disconnect();
});
</script>

<template>
  <!--
    Dock-right on desktop (`md:` ≥ 768px), fullscreen on mobile.
    The channel timeline behind the dock stays interactive on
    desktop so users can keep reading / typing while in a call —
    this was the "group chat not wired correctly to channels"
    complaint: the previous `fixed inset-0` overlay completely
    covered the room and made it impossible to use both at once.
  -->
  <aside
    v-if="state.phase === 'active'"
    class="fixed inset-0 z-40 flex flex-col bg-background/95 backdrop-blur-sm md:inset-y-0 md:left-auto md:right-0 md:w-[400px] md:border-l md:border-border"
    role="dialog"
    aria-label="Active call"
  >
    <header class="flex items-start justify-between gap-3 border-b border-border px-4 py-3">
      <div class="min-w-0 flex-1">
        <div class="type-chat-title truncate">{{ peerLabel }}</div>
        <div class="type-caption text-muted-foreground">
          <span v-if="connecting">Connecting…</span>
          <span v-else-if="state.kind === 'muc'">
            {{ remoteParticipantCount }} other{{ remoteParticipantCount === 1 ? "" : "s" }} in call
          </span>
          <span v-else>{{ state.media.video ? "Video call" : "Audio call" }}</span>
        </div>
      </div>
    </header>

    <div
      v-if="lastError"
      class="border-b border-destructive/30 bg-destructive/10 px-4 py-2 type-caption text-destructive"
      role="alert"
    >
      {{ lastError }}
    </div>

    <div class="grid flex-1 auto-rows-fr gap-2 overflow-y-auto p-3">
      <div
        v-for="tile in tiles"
        :key="tile.key"
        class="relative aspect-video w-full overflow-hidden rounded-lg border border-border bg-muted"
      >
        <!--
          Video element is always mounted for tiles that have or
          MIGHT have video (every tile, since the toggle can publish
          mid-call). Tiles without a current video track show the
          avatar placeholder on top via the v-if below; the <video>
          stays in the DOM so the ref callback fires the moment a
          track lands.
        -->
        <video
          v-if="tile.videoTrack"
          :ref="(el) => attachTrack(el as HTMLVideoElement | null, tile.videoTrack)"
          class="h-full w-full object-cover"
          :class="tile.isSelf ? 'scale-x-[-1]' : ''"
          autoplay
          playsinline
          :muted="tile.isSelf"
        />
        <div
          v-else
          class="flex h-full w-full items-center justify-center"
        >
          <AppAvatar :name="tile.label" size="lg" />
        </div>

        <audio
          v-if="tile.audioTrack && !tile.isSelf"
          :ref="(el) => attachTrack(el as HTMLAudioElement | null, tile.audioTrack)"
          autoplay
        />

        <div class="absolute inset-x-0 bottom-0 flex items-center justify-between gap-2 bg-gradient-to-t from-black/60 to-transparent px-2 py-1.5 text-white">
          <span class="type-caption truncate">{{ tile.label }}</span>
          <span
            v-if="tile.isSelf && !micEnabled"
            class="flex items-center"
            aria-label="Your mic is muted"
          >
            <MicOff class="h-3.5 w-3.5" />
          </span>
        </div>
      </div>
    </div>

    <footer class="flex items-center justify-center gap-2 border-t border-border px-4 py-3">
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
  </aside>
</template>
