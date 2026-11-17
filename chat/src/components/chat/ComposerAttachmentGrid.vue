<script setup lang="ts">
import { css } from "styled-system/css";
import { FileText, Music4, X } from "lucide-vue-next";
import { formatFileSize, type PendingAttachment } from "./composer-attachments";

defineProps<{
  attachments: PendingAttachment[];
}>();

const emit = defineEmits<{
  remove: [id: string];
}>();

// Spans the composer field's full width above the editor row.
const grid = css({ order: -2, flexBasis: "100%", display: "flex", flexWrap: "wrap", gap: "2", p: "1", animation: "wd-fade {durations.base} {easings.out} backwards" });
const tile = css({ position: "relative", overflow: "hidden", borderRadius: "control", border: "1px solid {colors.border}", bg: "surface.2" });
const thumb = css({ width: "80px", height: "80px", objectFit: "cover" });
const media = css({ width: "176px" });
const mediaFrame = css({ display: "block", width: "100%", height: "96px", bg: "bg", objectFit: "cover" });
const pdfFrame = css({ bg: "bg" });
const pdfFallback = css({ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: "2", height: "96px", textStyle: "caption", color: "fg.muted" });
const caption = css({ textStyle: "caption", px: "2", py: "1.5" });
const name = css({ fontWeight: 700, color: "fg", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const muted = css({ color: "fg.muted" });
const audioBox = css({ display: "flex", flexDirection: "column", gap: "2", width: "100%", maxWidth: "288px", p: "3" });
const audioTitle = css({ display: "flex", alignItems: "center", gap: "2", p: 0 });
const audio = css({ width: "100%", height: "36px" });
const glyph = css({ width: "18px", height: "18px", flex: "none", color: "action.text" });
const fileRow = css({ display: "flex", alignItems: "center", gap: "3", width: "100%", maxWidth: "288px", p: "3" });
const fileText = css({ minWidth: 0, flex: 1, fontSize: "sm" });
const removeBtn = css({
  position: "absolute",
  top: "1",
  right: "1",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  width: "24px",
  height: "24px",
  borderRadius: "pill",
  border: "1px solid {colors.border}",
  bg: "surface",
  color: "fg.muted",
  boxShadow: "elev.1",
  cursor: "pointer",
  opacity: 0,
  transition: "opacity {durations.fast} {easings.out}",
  "& svg": { width: "14px", height: "14px" },
  _hover: { color: "danger.text" },
  _focusVisible: { opacity: 1 },
  "div:hover > &": { opacity: 1 },
  _coarse: { opacity: 1 },
});
</script>

<template>
  <!-- Pending attachment previews -->
  <div :class="grid">
    <div
      v-for="att in attachments"
      :key="att.id"
      :class="tile"
    >
      <img
        v-if="att.previewKind === 'image'"
        :src="att.previewUrl"
        :alt="att.name"
        :class="thumb"
      />
      <div v-else-if="att.previewKind === 'video'" :class="media">
        <video
          :src="att.previewUrl"
          :class="mediaFrame"
          controls
          muted
          playsinline
          preload="metadata"
        />
        <div :class="caption">
          <div :class="name">{{ att.name }}</div>
          <div :class="muted">{{ formatFileSize(att.size) }}</div>
        </div>
      </div>
      <div v-else-if="att.previewKind === 'audio'" :class="audioBox">
        <div :class="[caption, audioTitle]">
          <Music4 :class="glyph" />
          <span :class="name">{{ att.name }}</span>
        </div>
        <audio :src="att.previewUrl" controls :class="audio" />
        <div :class="[caption, muted]">
          {{ att.mediaType }} · {{ formatFileSize(att.size) }}
        </div>
      </div>
      <div v-else-if="att.previewKind === 'pdf'" :class="media">
        <object
          :data="att.previewUrl"
          type="application/pdf"
          :class="[mediaFrame, pdfFrame]"
        >
          <div :class="pdfFallback">
            <FileText :class="glyph" />
            <span>PDF preview</span>
          </div>
        </object>
        <div :class="caption">
          <div :class="name">{{ att.name }}</div>
          <div :class="muted">{{ formatFileSize(att.size) }}</div>
        </div>
      </div>
      <div v-else :class="fileRow">
        <FileText :class="glyph" />
        <div :class="fileText">
          <div :class="name">{{ att.name }}</div>
          <div :class="[caption, muted, name]">
            {{ att.mediaType }} · {{ formatFileSize(att.size) }}
          </div>
        </div>
      </div>
      <button
        type="button"
        :class="removeBtn"
        :title="`Remove ${att.name}`"
        :aria-label="`Remove attachment ${att.name}`"
        @click="emit('remove', att.id)"
      >
        <X aria-hidden="true" />
      </button>
    </div>
  </div>
</template>
