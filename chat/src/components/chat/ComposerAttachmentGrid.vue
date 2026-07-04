<script setup lang="ts">
import { FileText, Music4, X } from "lucide-vue-next";
import { formatFileSize, type PendingAttachment } from "./composer-attachments";

defineProps<{
  attachments: PendingAttachment[];
}>();

const emit = defineEmits<{
  remove: [id: string];
}>();
</script>

<template>
  <!-- Pending attachment previews -->
  <div class="chat-composer-attachment-grid animate-fade-in">
    <div
      v-for="att in attachments"
      :key="att.id"
      class="relative group/att rounded-lg border border-border bg-muted overflow-hidden"
    >
      <img
        v-if="att.previewKind === 'image'"
        :src="att.previewUrl"
        :alt="att.name"
        class="h-20 w-20 object-cover"
      />
      <div v-else-if="att.previewKind === 'video'" class="w-44">
        <video
          :src="att.previewUrl"
          class="h-24 w-full bg-black object-cover"
          controls
          muted
          playsinline
          preload="metadata"
        />
        <div class="type-caption px-2 py-1.5">
          <div class="type-emphasis truncate text-foreground">{{ att.name }}</div>
          <div class="text-muted-foreground">{{ formatFileSize(att.size) }}</div>
        </div>
      </div>
      <div v-else-if="att.previewKind === 'audio'" class="flex w-full max-w-72 flex-col gap-2 p-3">
        <div class="type-caption type-emphasis flex items-center gap-2">
          <Music4 class="h-4 w-4 text-primary" />
          <span class="truncate">{{ att.name }}</span>
        </div>
        <audio :src="att.previewUrl" controls class="h-9 w-full" />
        <div class="type-caption text-muted-foreground">
          {{ att.mediaType }} · {{ formatFileSize(att.size) }}
        </div>
      </div>
      <div v-else-if="att.previewKind === 'pdf'" class="w-44">
        <object
          :data="att.previewUrl"
          type="application/pdf"
          class="h-24 w-full bg-background"
        >
          <div class="type-caption flex h-24 w-full flex-col items-center justify-center gap-2 text-muted-foreground">
            <FileText class="h-5 w-5 text-primary" />
            <span>PDF preview</span>
          </div>
        </object>
        <div class="type-caption px-2 py-1.5">
          <div class="type-emphasis truncate text-foreground">{{ att.name }}</div>
          <div class="text-muted-foreground">{{ formatFileSize(att.size) }}</div>
        </div>
      </div>
      <div v-else class="flex w-full max-w-72 items-center gap-3 p-3">
        <FileText class="h-5 w-5 flex-shrink-0 text-primary" />
        <div class="min-w-0 flex-1">
          <div class="type-control truncate text-foreground">{{ att.name }}</div>
          <div class="type-caption truncate text-muted-foreground">
            {{ att.mediaType }} · {{ formatFileSize(att.size) }}
          </div>
        </div>
      </div>
      <button
        type="button"
        class="absolute top-1 right-1 h-6 w-6 flex items-center justify-center rounded-full bg-background/90 text-muted-foreground hover:text-destructive border border-border shadow-sm opacity-0 group-hover/att:opacity-100 focus:opacity-100 transition-opacity"
        :title="`Remove ${att.name}`"
        :aria-label="`Remove attachment ${att.name}`"
        @click="emit('remove', att.id)"
      >
        <X class="w-3.5 h-3.5" aria-hidden="true" />
      </button>
    </div>
  </div>
</template>
