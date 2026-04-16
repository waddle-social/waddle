<script setup lang="ts">
import { ref, watch, onUnmounted, computed } from "vue";
import { Mic, MicOff, Video, VideoOff, PhoneOff, Minimize2, Maximize2 } from "lucide-vue-next";

const props = defineProps<{
  localStream: MediaStream | null;
  remoteParticipants: Map<string, { jid: string; stream: MediaStream }>;
  micEnabled: boolean;
  cameraEnabled: boolean;
  phase: string;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCamera: [];
  endCall: [];
}>();

interface CallTile {
  key: string;
  sourceKey: string;
  kind: "video" | "audio";
  label: string;
  playsInlineAudio: boolean;
  splitTrack?: MediaStreamTrack;
}

interface HiddenAudioFeed {
  key: string;
  sourceKey: string;
}

const collapsed = ref(false);
const localVideoRef = ref<HTMLVideoElement | null>(null);
const remoteVideoRefs = ref<Map<string, HTMLVideoElement>>(new Map());
const hiddenAudioRefs = ref<Map<string, HTMLAudioElement>>(new Map());
const splitVideoStreams = ref<Map<string, MediaStream>>(new Map());

function liveTracks(tracks: MediaStreamTrack[]) {
  return tracks.filter((track) => track.readyState === "live");
}

function participantStream(sourceKey: string) {
  return props.remoteParticipants.get(sourceKey)?.stream ?? null;
}

function splitVideoStream(track: MediaStreamTrack) {
  let stream = splitVideoStreams.value.get(track.id);
  if (!stream) {
    stream = new MediaStream([track]);
    splitVideoStreams.value.set(track.id, stream);
  }
  return stream;
}

const stageTiles = computed<CallTile[]>(() => {
  const tiles: CallTile[] = [];
  let guestIndex = 1;
  let listenerIndex = 1;

  for (const [sourceKey, participant] of props.remoteParticipants) {
    const videoTracks = liveTracks(participant.stream.getVideoTracks());
    const audioTracks = liveTracks(participant.stream.getAudioTracks());

    if (videoTracks.length === 0) {
      tiles.push({
        key: `${sourceKey}:audio`,
        sourceKey,
        kind: "audio",
        label: `Listener ${listenerIndex++}`,
        playsInlineAudio: false,
      });
      continue;
    }

    if (videoTracks.length === 1) {
      tiles.push({
        key: `${sourceKey}:video:${videoTracks[0]!.id}`,
        sourceKey,
        kind: "video",
        label: `Guest ${guestIndex++}`,
        playsInlineAudio: audioTracks.length > 0,
      });
      continue;
    }

    for (const track of videoTracks) {
      tiles.push({
        key: `${sourceKey}:video:${track.id}`,
        sourceKey,
        kind: "video",
        label: `Guest ${guestIndex++}`,
        playsInlineAudio: false,
        splitTrack: track,
      });
    }
  }

  return tiles;
});

const hiddenAudioFeeds = computed<HiddenAudioFeed[]>(() => {
  const feeds: HiddenAudioFeed[] = [];

  for (const [sourceKey, participant] of props.remoteParticipants) {
    const videoTracks = liveTracks(participant.stream.getVideoTracks());
    const audioTracks = liveTracks(participant.stream.getAudioTracks());

    if (audioTracks.length === 0) continue;
    if (videoTracks.length !== 1) {
      feeds.push({
        key: `${sourceKey}:audio-feed`,
        sourceKey,
      });
    }
  }

  return feeds;
});

const participantCount = computed(() => stageTiles.value.length + (props.localStream ? 1 : 0));
const phaseLabel = computed(() => props.phase.replaceAll("-", " "));

const gridCols = computed(() => {
  const count = stageTiles.value.length;
  if (count <= 1) return "grid-cols-1";
  if (count <= 4) return "grid-cols-2";
  return "grid-cols-3";
});

watch(
  () => props.localStream,
  (stream) => {
    if (localVideoRef.value) {
      localVideoRef.value.srcObject = stream;
    }
  },
);

function syncRemoteVideoElement(tile: CallTile, el: HTMLVideoElement) {
  const stream = tile.splitTrack
    ? splitVideoStream(tile.splitTrack)
    : participantStream(tile.sourceKey);

  if (el.srcObject !== stream) {
    el.srcObject = stream;
  }
  el.muted = !tile.playsInlineAudio;
}

function syncHiddenAudioElement(feed: HiddenAudioFeed, el: HTMLAudioElement) {
  const stream = participantStream(feed.sourceKey);
  if (el.srcObject !== stream) {
    el.srcObject = stream;
  }
}

function setRemoteVideoRef(tile: CallTile, el: HTMLVideoElement | null) {
  if (el) {
    remoteVideoRefs.value.set(tile.key, el);
    syncRemoteVideoElement(tile, el);
  } else {
    remoteVideoRefs.value.delete(tile.key);
  }
}

function setHiddenAudioRef(feed: HiddenAudioFeed, el: HTMLAudioElement | null) {
  if (el) {
    hiddenAudioRefs.value.set(feed.key, el);
    syncHiddenAudioElement(feed, el);
  } else {
    hiddenAudioRefs.value.delete(feed.key);
  }
}

watch(
  stageTiles,
  (tiles) => {
    const tileMap = new Map(tiles.map((tile) => [tile.key, tile]));
    const activeSplitTracks = new Set(
      tiles
        .map((tile) => tile.splitTrack?.id)
        .filter((trackId): trackId is string => !!trackId),
    );

    for (const trackId of splitVideoStreams.value.keys()) {
      if (!activeSplitTracks.has(trackId)) {
        splitVideoStreams.value.delete(trackId);
      }
    }

    for (const [key, el] of remoteVideoRefs.value) {
      const tile = tileMap.get(key);
      if (tile) {
        syncRemoteVideoElement(tile, el);
      } else {
        el.srcObject = null;
      }
    }
  },
  { immediate: true },
);

watch(
  hiddenAudioFeeds,
  (feeds) => {
    const feedMap = new Map(feeds.map((feed) => [feed.key, feed]));

    for (const [key, el] of hiddenAudioRefs.value) {
      const feed = feedMap.get(key);
      if (feed) {
        syncHiddenAudioElement(feed, el);
      } else {
        el.srcObject = null;
      }
    }
  },
  { immediate: true },
);

onUnmounted(() => {
  if (localVideoRef.value) {
    localVideoRef.value.srcObject = null;
  }
  for (const el of remoteVideoRefs.value.values()) {
    el.srcObject = null;
  }
  for (const el of hiddenAudioRefs.value.values()) {
    el.srcObject = null;
  }
  splitVideoStreams.value.clear();
});
</script>

<template>
  <div class="border-b border-border bg-background">
    <!-- Collapsed mode -->
    <div
      v-if="collapsed"
      class="px-6 h-11 flex items-center justify-between glass-surface"
    >
      <span class="text-xs text-muted-foreground flex items-center gap-2">
        <span class="w-2 h-2 rounded-full bg-success shadow-[0_0_6px_var(--success)] animate-pulse" />
        Call · {{ participantCount }} on call · {{ phaseLabel }}
      </span>
      <div class="flex items-center gap-1">
        <button
          class="h-8 w-8 flex items-center justify-center rounded-lg hover:bg-muted transition-all duration-200"
          title="Expand call"
          @click="collapsed = false"
        >
          <Maximize2 class="w-3.5 h-3.5" />
        </button>
        <button
          class="h-8 w-8 flex items-center justify-center rounded-lg text-destructive hover:bg-destructive/10 transition-all duration-200"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <!-- Expanded mode -->
    <div v-else class="flex flex-col">
      <div class="border-t border-border/60 px-4 py-4 sm:px-6 sm:py-5">
        <div class="relative overflow-hidden rounded-[28px] border border-border/70 bg-muted/20 p-3 sm:p-4">
          <div
            v-if="stageTiles.length === 0"
            class="flex h-[clamp(18rem,54vh,40rem)] items-center justify-center rounded-[24px] border border-dashed border-border/70 bg-background/30"
          >
            <div class="text-center">
              <div class="mb-2 flex items-center justify-center gap-1.5">
                <span class="typing-dot" />
                <span class="typing-dot" />
                <span class="typing-dot" />
              </div>
              <p class="text-sm font-medium text-foreground">Waiting for others to join</p>
              <p class="mt-1 text-xs text-muted-foreground">Your preview stays tucked away while the stage stays ready.</p>
            </div>
          </div>

          <div
            v-else
            class="grid h-[clamp(18rem,54vh,40rem)] auto-rows-fr gap-3"
            :class="gridCols"
          >
            <article
              v-for="tile in stageTiles"
              :key="tile.key"
              class="relative min-h-[11rem] overflow-hidden rounded-[24px] border border-border/70 bg-background/60"
            >
              <video
                v-if="tile.kind === 'video'"
                :ref="(el) => setRemoteVideoRef(tile, el as HTMLVideoElement | null)"
                autoplay
                playsinline
                class="absolute inset-0 h-full w-full object-cover"
              />

              <div
                v-else
                class="absolute inset-0 flex flex-col items-center justify-center gap-3 bg-[radial-gradient(circle_at_top,rgba(255,255,255,0.06),transparent_55%)] text-center"
              >
                <div class="flex h-14 w-14 items-center justify-center rounded-full bg-background/80 text-foreground shadow-sm">
                  <Mic class="h-6 w-6" />
                </div>
                <div>
                  <p class="text-sm font-semibold text-foreground">{{ tile.label }}</p>
                  <p class="mt-1 text-xs text-muted-foreground">Voice only</p>
                </div>
              </div>

              <div class="absolute inset-x-0 bottom-0 flex items-center justify-between gap-3 bg-gradient-to-t from-background/90 via-background/30 to-transparent p-4">
                <span class="rounded-full bg-background/80 px-3 py-1 text-xs font-medium tracking-wide text-foreground backdrop-blur-sm">
                  {{ tile.label }}
                </span>
                <span
                  v-if="tile.kind === 'audio'"
                  class="rounded-full bg-background/70 px-2.5 py-1 text-[11px] font-medium uppercase tracking-[0.14em] text-muted-foreground backdrop-blur-sm"
                >
                  Audio
                </span>
              </div>
            </article>
          </div>

          <div class="pointer-events-none absolute bottom-4 right-4 w-40 max-w-[32%] overflow-hidden rounded-[20px] border border-border/70 bg-background/80 shadow-xl backdrop-blur-sm">
            <video
              ref="localVideoRef"
              autoplay
              playsinline
              muted
              class="h-full w-full aspect-video object-cover"
              style="transform: scaleX(-1)"
            />
            <div class="absolute left-3 top-3 rounded-full bg-background/80 px-2.5 py-1 text-[11px] font-medium tracking-wide text-foreground">
              You
            </div>
          </div>

          <audio
            v-for="feed in hiddenAudioFeeds"
            :key="feed.key"
            :ref="(el) => setHiddenAudioRef(feed, el as HTMLAudioElement | null)"
            autoplay
            playsinline
            class="hidden"
          />
        </div>
      </div>

      <!-- Controls bar -->
      <div class="px-6 py-3 flex items-center justify-center gap-2.5 border-t border-border glass-surface">
        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl transition-all duration-200"
          :class="micEnabled
            ? 'bg-muted hover:bg-muted/80'
            : 'bg-destructive/10 text-destructive hover:bg-destructive/20'"
          :title="micEnabled ? 'Mute microphone' : 'Unmute microphone'"
          @click="emit('toggleMic')"
        >
          <Mic v-if="micEnabled" class="w-4 h-4" />
          <MicOff v-else class="w-4 h-4" />
        </button>

        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl transition-all duration-200"
          :class="cameraEnabled
            ? 'bg-muted hover:bg-muted/80'
            : 'bg-destructive/10 text-destructive hover:bg-destructive/20'"
          :title="cameraEnabled ? 'Disable camera' : 'Enable camera'"
          @click="emit('toggleCamera')"
        >
          <Video v-if="cameraEnabled" class="w-4 h-4" />
          <VideoOff v-else class="w-4 h-4" />
        </button>

        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl bg-destructive text-white hover:shadow-[0_0_16px_rgba(239,68,68,0.4)] transition-all duration-200"
          title="End call"
          @click="emit('endCall')"
        >
          <PhoneOff class="w-4 h-4" />
        </button>

        <button
          class="h-10 w-10 flex items-center justify-center rounded-xl bg-muted hover:bg-muted/80 transition-all duration-200"
          title="Minimize call"
          @click="collapsed = true"
        >
          <Minimize2 class="w-4 h-4" />
        </button>
      </div>
    </div>
  </div>
</template>
