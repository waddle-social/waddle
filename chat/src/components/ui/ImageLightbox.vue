<script setup lang="ts">
import { computed, onBeforeUnmount, watch } from "vue";
import { X, ChevronLeft, ChevronRight, Download } from "lucide-vue-next";

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
      class="z-lightbox fixed inset-0 animate-fade-in flex items-center justify-center"
      role="dialog"
      aria-modal="true"
    >
      <div class="absolute inset-0 bg-background/85 backdrop-blur-md" @click="close" />

      <div class="z-sticky absolute top-3 right-3 flex items-center gap-1">
        <a
          :href="current.url"
          :download="current.name ?? ''"
          target="_blank"
          rel="noopener noreferrer"
          class="chat-lightbox-control h-9 w-9"
          title="Download"
          aria-label="Download original"
          @click.stop
        >
          <Download class="w-4 h-4" />
        </a>
        <button
          class="chat-lightbox-control h-9 w-9"
          type="button"
          title="Close (Esc)"
          aria-label="Close lightbox"
          @click="close"
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      <button
        v-if="hasPrev"
        class="chat-lightbox-control z-sticky absolute left-3 top-1/2 -translate-y-1/2 h-10 w-10"
        type="button"
        title="Previous (←)"
        aria-label="Previous image"
        @click.stop="prev"
      >
        <ChevronLeft class="w-5 h-5" />
      </button>
      <button
        v-if="hasNext"
        class="chat-lightbox-control z-sticky absolute right-3 top-1/2 -translate-y-1/2 h-10 w-10"
        type="button"
        title="Next (→)"
        aria-label="Next image"
        @click.stop="next"
      >
        <ChevronRight class="w-5 h-5" />
      </button>

      <div class="chat-lightbox-frame relative flex flex-col items-center justify-center" @click.stop>
        <img
          :src="current.url"
          :alt="current.name ?? 'Image'"
          class="chat-lightbox-image object-contain rounded-lg shadow-2xl animate-slide-up"
        />
        <div
          v-if="current.name || images.length > 1"
          class="type-caption mt-3 flex items-center gap-2 text-muted-foreground"
        >
          <span v-if="current.name" class="chat-lightbox-caption truncate">{{ current.name }}</span>
          <span v-if="images.length > 1" class="type-numeric">{{ index + 1 }} / {{ images.length }}</span>
        </div>
      </div>
    </div>
  </Teleport>
</template>
