<script setup lang="ts">
import { onBeforeUnmount } from "vue";
import { css } from "styled-system/css";
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

// HuddleStage "sharing" strip: danger tint, red dot, your screen as a
// thumbnail so you can see what everyone sees.
const rootClass = css({
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "2.5",
  margin: "0 {spacing.4} {spacing.2.5}",
  padding: "{spacing.2} {spacing.2} {spacing.2} {spacing.3}",
  borderRadius: "12px",
  bg: "color-mix(in srgb, {colors.danger} 14%, transparent)",
  border: "1px solid color-mix(in srgb, {colors.danger} 40%, transparent)",
  color: "fg",
  _narrow: { mx: "3" },
});
const copyClass = css({ minWidth: 0, display: "inline-flex", alignItems: "center", gap: "2.5", font: "600 13px/1.3 {fonts.body}" });
const dotClass = css({ width: "8px", height: "8px", flex: "none", borderRadius: "pill", bg: "danger" });
const thumbClass = css({ width: "clamp(96px, 16vw, 144px)", aspectRatio: "16 / 9", flex: "0 0 auto", "@media (max-height: 620px)": { width: "60px" } });
const thumbCompactClass = css({ width: "clamp(64px, 12vw, 96px)", aspectRatio: "16 / 9", flex: "0 0 auto", "@media (max-height: 620px)": { width: "60px" } });
</script>

<template>
  <aside
    :class="rootClass"
    role="status"
    aria-live="polite"
  >
    <div :class="copyClass">
      <span :class="dotClass" aria-hidden="true" />
      <span>{{ presentation.message }}</span>
    </div>
    <div :class="compact ? thumbCompactClass : thumbClass">
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
    />
    </div>
  </aside>
</template>
