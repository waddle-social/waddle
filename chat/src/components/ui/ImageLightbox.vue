<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from "vue";
import { X, ChevronLeft, ChevronRight, Download } from "lucide-vue-next";

export interface LightboxImage {
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

const current = computed(() => props.images[props.index] ?? null);
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
    <div
      v-if="open && current"
      class="fixed inset-0 z-[60] animate-fade-in flex items-center justify-center"
      role="dialog"
      aria-modal="true"
    >
      <div class="absolute inset-0 bg-background/85 backdrop-blur-md" @click="close" />

      <div class="absolute top-3 right-3 z-10 flex items-center gap-1">
        <a
          :href="current.url"
          :download="current.name ?? ''"
          target="_blank"
          rel="noopener noreferrer"
          class="h-9 w-9 flex items-center justify-center rounded-full bg-card/80 backdrop-blur border border-border text-foreground hover:bg-card transition-colors"
          title="Open original"
          @click.stop
        >
          <Download class="w-4 h-4" />
        </a>
        <button
          class="h-9 w-9 flex items-center justify-center rounded-full bg-card/80 backdrop-blur border border-border text-foreground hover:bg-card transition-colors"
          title="Close"
          @click="close"
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      <button
        v-if="hasPrev"
        class="absolute left-3 top-1/2 -translate-y-1/2 z-10 h-10 w-10 flex items-center justify-center rounded-full bg-card/80 backdrop-blur border border-border text-foreground hover:bg-card transition-colors"
        title="Previous"
        @click.stop="prev"
      >
        <ChevronLeft class="w-5 h-5" />
      </button>
      <button
        v-if="hasNext"
        class="absolute right-3 top-1/2 -translate-y-1/2 z-10 h-10 w-10 flex items-center justify-center rounded-full bg-card/80 backdrop-blur border border-border text-foreground hover:bg-card transition-colors"
        title="Next"
        @click.stop="next"
      >
        <ChevronRight class="w-5 h-5" />
      </button>

      <div class="relative max-w-[92vw] max-h-[90vh] flex flex-col items-center justify-center" @click.stop>
        <img
          :src="current.url"
          :alt="current.name ?? 'Image'"
          class="max-w-[92vw] max-h-[82vh] object-contain rounded-xl shadow-2xl animate-slide-up"
        />
        <div
          v-if="current.name || images.length > 1"
          class="mt-3 text-[12px] text-muted-foreground tabular-nums flex items-center gap-2"
        >
          <span v-if="current.name" class="truncate max-w-[60vw]">{{ current.name }}</span>
          <span v-if="images.length > 1" class="font-mono">{{ index + 1 }} / {{ images.length }}</span>
        </div>
      </div>
    </div>
  </Teleport>
</template>
