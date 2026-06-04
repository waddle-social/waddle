<script setup lang="ts">
/**
 * The actual <video> for a native-video link preview. Mounted only after the
 * user clicks play (the parent gates it), so no element or network fetch exists
 * before that. Owns the playback attachment for its lifetime: progressive media
 * and native-HLS browsers use the <video> src; HLS elsewhere lazily loads
 * hls.js. A fatal playback error emits `failed` so the parent can show a link.
 */
import { onMounted, onUnmounted, ref } from "vue";
import { attachNativeVideo, type VideoAttachment } from "@/lib/xmpp/hls-player";

const props = defineProps<{ url: string; mediaType: string }>();
const emit = defineEmits<{ failed: [] }>();

const videoEl = ref<HTMLVideoElement | null>(null);
let attachment: VideoAttachment | null = null;
let unmounted = false;

onMounted(async () => {
  const el = videoEl.value;
  if (!el) return;
  const handle = await attachNativeVideo(el, props.url, props.mediaType, () => emit("failed"));
  // The component may have unmounted while hls.js was importing; never retain a
  // handle (worker + segment fetches) for a torn-down element.
  if (unmounted) {
    handle.destroy();
    return;
  }
  attachment = handle;
});

onUnmounted(() => {
  unmounted = true;
  attachment?.destroy();
  attachment = null;
});
</script>

<template>
  <video
    ref="videoEl"
    class="max-h-72 w-full rounded border border-border bg-black"
    controls
    autoplay
    playsinline
    @click.stop
  />
</template>
