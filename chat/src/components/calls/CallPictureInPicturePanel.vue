<script setup lang="ts">
import { onBeforeUnmount } from "vue";
import { Mic, MicOff, PhoneOff, RotateCcw, Video, VideoOff } from "lucide-vue-next";
import type { CallTileModel } from "@/lib/calls/call-tiles";
import { TileAttachments, type TileAttachable } from "@/lib/calls/tile-attach";
import CallTile from "./CallTile.vue";

const props = defineProps<{
  tile: CallTileModel | null;
  micEnabled: boolean;
  camEnabled: boolean;
}>();

const emit = defineEmits<{
  toggleMic: [];
  toggleCam: [];
  hangup: [];
  returnToCall: [];
}>();

const attachments = new TileAttachments();

function attach(
  key: string,
  el: HTMLMediaElement | null,
  track: TileAttachable | null,
): void {
  attachments.sync(key, el, track);
}

onBeforeUnmount(() => {
  attachments.detachAll();
});
</script>

<template>
  <section class="call-pip-panel" aria-label="Call Picture-in-Picture">
    <div class="call-pip-panel__tile">
      <CallTile
        v-if="tile"
        :key="tile.key"
        :label="tile.label"
        :attach-key="`pip:${tile.key}`"
        :is-self="tile.isSelf"
        :mirror-video="tile.mirrorVideo"
        :shows-presenting-glyph="tile.showsPresentingGlyph"
        :mic-enabled="tile.micEnabledHint"
        :video-track="tile.videoTrack"
        :attach="attach"
        :interactive="false"
      />
      <div v-else class="call-pip-panel__empty">No active video</div>
    </div>
    <div class="call-pip-panel__controls">
      <button
        type="button"
        class="call-pip-panel__button"
        :aria-label="micEnabled ? 'Mute' : 'Unmute'"
        :title="micEnabled ? 'Mute' : 'Unmute'"
        @click="emit('toggleMic')"
      >
        <component :is="micEnabled ? Mic : MicOff" class="w-4 h-4" />
      </button>
      <button
        type="button"
        class="call-pip-panel__button"
        :aria-label="camEnabled ? 'Camera off' : 'Camera on'"
        :title="camEnabled ? 'Camera off' : 'Camera on'"
        @click="emit('toggleCam')"
      >
        <component :is="camEnabled ? Video : VideoOff" class="w-4 h-4" />
      </button>
      <button
        type="button"
        class="call-pip-panel__button"
        aria-label="Return to call"
        title="Return to call"
        @click="emit('returnToCall')"
      >
        <RotateCcw class="w-4 h-4" />
      </button>
      <button
        type="button"
        class="call-pip-panel__button call-pip-panel__button--danger"
        aria-label="Hang up"
        title="Hang up"
        @click="emit('hangup')"
      >
        <PhoneOff class="w-4 h-4" />
      </button>
    </div>
  </section>
</template>

<style scoped>
.call-pip-panel {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  width: 100vw;
  height: 100vh;
  padding: 0.5rem;
  background: var(--background);
  color: var(--foreground);
  box-sizing: border-box;
}

.call-pip-panel__tile {
  flex: 1 1 0%;
  min-height: 0;
}

.call-pip-panel__empty {
  display: grid;
  place-items: center;
  width: 100%;
  height: 100%;
  border-radius: var(--radius-md);
  background: var(--muted);
  color: var(--muted-foreground);
}

.call-pip-panel__controls {
  display: flex;
  justify-content: center;
  gap: 0.375rem;
}

.call-pip-panel__button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 2rem;
  height: 2rem;
  border-radius: var(--radius-sm);
  background: var(--secondary);
  color: var(--secondary-foreground);
}

.call-pip-panel__button:hover,
.call-pip-panel__button:focus-visible {
  background: var(--muted);
}

.call-pip-panel__button--danger {
  background: var(--destructive);
  color: var(--destructive-foreground);
}
</style>
