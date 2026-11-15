<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from "vue";
import { X, ChevronLeft, ChevronRight, Download } from "lucide-vue-next";
import { safeAttachmentUrl } from "@/lib/attachment-url";
import { css, cx } from "styled-system/css";
import AppTooltip from "@/components/ui/AppTooltip.vue";

interface LightboxImage {
  url: string;
  name?: string;
  width?: number;
  height?: number;
}

const props = defineProps<{
  open: boolean;
  images: LightboxImage[];
  index: number;
}>();

const emit = defineEmits<{
  "update:open": [value: boolean];
  "update:index": [value: number];
}>();

// The stage is dark in both themes, like a video surface.
const rootClass = css({ position: "fixed", inset: 0, zIndex: "dialog", display: "flex", alignItems: "center", justifyContent: "center", animation: "wd-fade {durations.base} {easings.out} backwards" });
const backdropClass = css({ position: "absolute", inset: 0, border: 0, bg: "rgba(2, 9, 18, 0.92)", cursor: "zoom-out" });
const toolbarClass = css({ position: "absolute", top: "3", right: "3", zIndex: "sticky", display: "flex", alignItems: "center", gap: "1" });
const controlClass = css({
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  width: "44px",
  height: "44px",
  border: 0,
  borderRadius: "pill",
  bg: "material.media",
  color: "#f3f5f7",
  backdropFilter: "blur(20px) saturate(160%)",
  cursor: "pointer",
  transition: "background-color {durations.fast} {easings.out}",
  "& svg": { width: "20px", height: "20px" },
  _hover: { bg: "rgba(7, 12, 18, 0.9)" },
});
const sideClass = css({ position: "absolute", top: "50%", translate: "0 -50%", zIndex: "sticky" });
const prevClass = cx(controlClass, sideClass, css({ left: "3" }));
const nextClass = cx(controlClass, sideClass, css({ right: "3" }));
const frameClass = css({ position: "relative", display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", maxWidth: "min(92vw, 96rem)", maxHeight: "90dvh" });
const imageClass = css({ maxWidth: "min(92vw, 96rem)", maxHeight: "82dvh", objectFit: "contain", borderRadius: "panel", boxShadow: "elev.3", animation: "wd-pop {durations.slow} {easings.spring.snappy} backwards" });
const captionRowClass = css({ mt: "3", display: "flex", alignItems: "center", gap: "2", textStyle: "caption", color: "#c4ccd6" });
const captionClass = css({ display: "block", minWidth: 0, maxWidth: "min(60vw, 42rem)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });

const current = computed(() => props.images[props.index] ?? null);
// Callers already resolve gallery entries through the attachment scheme
// allowlist; re-checking here keeps the lightbox safe against any future
// caller that forgets to.
const currentUrl = computed(() => safeAttachmentUrl(current.value?.url));
const hasPrev = computed(() => props.index > 0);
const hasNext = computed(() => props.index < props.images.length - 1);

function close() {
  emit("update:open", false);
}

function prev() {
  if (hasPrev.value) emit("update:index", props.index - 1);
}

function next() {
  if (hasNext.value) emit("update:index", props.index + 1);
}

function onKeydown(e: KeyboardEvent) {
  if (!props.open) return;
  if (e.key === "Escape") close();
  else if (e.key === "ArrowLeft") prev();
  else if (e.key === "ArrowRight") next();
}

watch(
  () => props.open,
  (open) => {
    if (typeof window === "undefined") return;
    if (open) window.addEventListener("keydown", onKeydown);
    else window.removeEventListener("keydown", onKeydown);
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  if (typeof window !== "undefined") window.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <Teleport to="body">
    <div v-if="open && current" :class="rootClass" role="dialog" aria-modal="true" data-theme="night">
      <button type="button" :class="backdropClass" aria-label="Close image" @click="close" />

      <div :class="toolbarClass">
        <AppTooltip v-if="currentUrl" label="Download" placement="bottom">
          <a
            :href="currentUrl"
            :download="current.name ?? ''"
            target="_blank"
            rel="noopener noreferrer"
            :class="controlClass"
            aria-label="Download original"
            @click.stop
          >
            <Download aria-hidden="true" />
          </a>
        </AppTooltip>
        <AppTooltip label="Close (Esc)" placement="bottom">
          <button :class="controlClass" type="button" aria-label="Close lightbox" @click="close">
            <X aria-hidden="true" />
          </button>
        </AppTooltip>
      </div>

      <AppTooltip v-if="hasPrev" label="Previous (←)" placement="right">
        <button :class="prevClass" type="button" aria-label="Previous image" @click.stop="prev">
          <ChevronLeft aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip v-if="hasNext" label="Next (→)" placement="left">
        <button :class="nextClass" type="button" aria-label="Next image" @click.stop="next">
          <ChevronRight aria-hidden="true" />
        </button>
      </AppTooltip>

      <div :class="frameClass" @click.stop>
        <img v-if="currentUrl" :src="currentUrl" :alt="current.name ?? 'Image'" :class="imageClass" />
        <div v-if="current.name || images.length > 1" :class="captionRowClass">
          <span v-if="current.name" :class="captionClass">{{ current.name }}</span>
          <span v-if="images.length > 1">{{ index + 1 }} / {{ images.length }}</span>
        </div>
      </div>
    </div>
  </Teleport>
</template>
