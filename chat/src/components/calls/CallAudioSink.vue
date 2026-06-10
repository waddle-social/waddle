<script setup lang="ts">
import { computed, onBeforeUnmount } from "vue";
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

function refAudio(track: CallAudioSinkTrack, el: Element | null): void {
  attachments.sync(track, el instanceof HTMLAudioElement ? el : null);
}

onBeforeUnmount(() => {
  attachments.detachAll();
});
</script>

<template>
  <div class="call-audio-sink" aria-hidden="true">
    <audio
      v-for="track in audioTracks"
      :key="callAudioSinkTrackKey(track)"
      :ref="(el) => refAudio(track, el)"
      autoplay
    />
  </div>
</template>

<style scoped>
.call-audio-sink {
  display: none;
}
</style>
