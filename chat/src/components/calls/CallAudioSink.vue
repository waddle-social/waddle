<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from "vue";
import {
  CallAudioSinkAttachments,
  callAudioSinkTrackKey,
  callAudioSinkTracks,
  type CallAudioSinkTrack,
} from "@/lib/calls/call-audio-sink";
import { useCallEngine } from "@/lib/calls/use-call-engine";

const { remoteTracks } = useCallEngine();
const audioTracks = computed(() => callAudioSinkTracks(remoteTracks.value));
const attachments = new CallAudioSinkAttachments();
const audioRefs = new Map<string, { track: CallAudioSinkTrack; ref: (el: Element | null) => void }>();

function audioRef(track: CallAudioSinkTrack): (el: Element | null) => void {
  const key = callAudioSinkTrackKey(track);
  const existing = audioRefs.get(key);
  if (existing?.track === track) return existing.ref;
  const ref = (el: Element | null) => {
    attachments.sync(track, el instanceof HTMLAudioElement ? el : null);
  };
  audioRefs.set(key, { track, ref });
  return ref;
}

watch(audioTracks, (tracks) => {
  const liveKeys = new Set(tracks.map(callAudioSinkTrackKey));
  for (const key of audioRefs.keys()) {
    if (!liveKeys.has(key)) audioRefs.delete(key);
  }
});

onBeforeUnmount(() => {
  attachments.detachAll();
  audioRefs.clear();
});
</script>

<template>
  <div class="call-audio-sink" aria-hidden="true">
    <audio
      v-for="track in audioTracks"
      :key="callAudioSinkTrackKey(track)"
      :ref="audioRef(track)"
      autoplay
    />
  </div>
</template>

<style scoped>
.call-audio-sink {
  display: none;
}
</style>
