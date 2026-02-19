<script setup lang="ts">
import { computed, ref } from 'vue';

const props = withDefaults(defineProps<{
  /** Bare JID for fallback color/initials */
  jid: string;
  /** Display name (for initials) */
  name?: string;
  /** Photo URL from vCard (EXTVAL or data: URI) */
  photoUrl?: string | null;
  /** Size in pixels */
  size?: number;
}>(), {
  name: '',
  photoUrl: null,
  size: 32,
});

const imgFailed = ref(false);

const showImage = computed(() => !!props.photoUrl && !imgFailed.value);

const initials = computed(() => {
  const source = props.name || props.jid;
  const parts = source.split(/[@.\s]+/).filter(Boolean);
  if (parts.length >= 2) {
    return `${parts[0]?.[0] ?? ''}${parts[1]?.[0] ?? ''}`.toUpperCase();
  }
  return source.slice(0, 2).toUpperCase();
});

const bgColor = computed(() => {
  const colors = ['#5865f2', '#57f287', '#fee75c', '#eb459e', '#ed4245', '#3ba55c', '#faa61a', '#e67e22'];
  let hash = 0;
  for (const ch of props.jid) hash = ch.charCodeAt(0) + ((hash << 5) - hash);
  return colors[Math.abs(hash) % colors.length] ?? '#5865f2';
});

const sizeStyle = computed(() => ({
  width: `${props.size}px`,
  height: `${props.size}px`,
  fontSize: `${Math.max(10, props.size * 0.38)}px`,
}));

function onImgError() {
  imgFailed.value = true;
}
</script>

<template>
  <div
    class="relative flex flex-shrink-0 items-center justify-center overflow-hidden rounded-full font-semibold text-white"
    :style="{ ...sizeStyle, backgroundColor: showImage ? 'transparent' : bgColor }"
  >
    <img
      v-if="showImage"
      :src="photoUrl!"
      :alt="name || jid"
      class="absolute inset-0 h-full w-full object-cover"
      loading="lazy"
      @error="onImgError"
    />
    <span v-else>{{ initials }}</span>
  </div>
</template>
