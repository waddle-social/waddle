<script setup lang="ts">
import { onBeforeUnmount } from "vue";
import type { LocalScreenSharePresentation } from "@/lib/calls/call-self-share";
import { TileAttachments, type TileAttachable } from "@/lib/calls/tile-attach";
import CallTile from "./CallTile.vue";

defineProps<{
  presentation: LocalScreenSharePresentation;
  compact?: boolean;
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
  <aside
    class="call-self-share-notice"
    :class="{ 'call-self-share-notice--compact': compact }"
    role="status"
    aria-live="polite"
  >
    <div class="call-self-share-notice__copy">
      <span class="call-self-share-notice__dot" aria-hidden="true" />
      <span class="type-control">{{ presentation.message }}</span>
    </div>
    <CallTile
      :label="presentation.thumbnail.label"
      :attach-key="presentation.thumbnail.attachKey"
      :is-self="true"
      :mirror-video="presentation.thumbnail.mirrorVideo"
      :shows-presenting-glyph="true"
      :mic-enabled="true"
      :video-track="presentation.thumbnail.videoTrack"
      :audio-track="null"
      :attach="attach"
      :interactive="false"
      class="call-self-share-notice__thumb"
    />
  </aside>
</template>

<style scoped>
.call-self-share-notice {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-sm);
  border-bottom: 1px solid var(--border);
  background: color-mix(in oklab, var(--primary) 9%, var(--background));
  padding: 0.5rem 0.75rem;
}

.call-self-share-notice__copy {
  min-width: 0;
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  color: var(--foreground);
}

.call-self-share-notice__dot {
  width: 0.55rem;
  height: 0.55rem;
  flex: 0 0 auto;
  border-radius: 9999px;
  background: var(--primary);
  box-shadow: 0 0 0 4px color-mix(in oklab, var(--primary) 18%, transparent);
}

.call-self-share-notice__thumb {
  width: clamp(6.5rem, 18vw, 9rem);
  height: auto;
  aspect-ratio: 16 / 9;
  flex: 0 0 auto;
}

.call-self-share-notice--compact {
  padding-block: 0.35rem;
}

.call-self-share-notice--compact .call-self-share-notice__thumb {
  width: clamp(4.5rem, 14vw, 6rem);
}

@media (max-height: 620px) {
  .call-self-share-notice--compact .call-self-share-notice__thumb {
    width: 3.75rem;
  }
}

@media (max-width: 520px) {
  .call-self-share-notice {
    align-items: stretch;
    flex-direction: column;
  }

  .call-self-share-notice__thumb {
    width: min(100%, 10rem);
  }

  .call-self-share-notice--compact {
    align-items: center;
    flex-direction: row;
  }

  .call-self-share-notice--compact .call-self-share-notice__thumb {
    width: 5rem;
  }
}

@media (max-width: 360px) {
  .call-self-share-notice--compact .call-self-share-notice__thumb {
    width: 3.75rem;
  }
}
</style>
