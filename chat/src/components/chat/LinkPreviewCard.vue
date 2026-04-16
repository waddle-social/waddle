<script setup lang="ts">
import { computed } from "vue";
import type { WaddleLinkPreview } from "@/lib/xmpp/extensions/preview";

const props = defineProps<{ preview: WaddleLinkPreview }>();

const target = computed(() => props.preview.url);

const hasTitle = computed(() => !!props.preview.title?.trim());
</script>

<template>
  <a
    v-if="hasTitle"
    :href="target"
    target="_blank"
    rel="noopener noreferrer"
    class="mt-2 block rounded-xl border border-border bg-muted/40 hover:bg-muted transition-colors duration-200 overflow-hidden max-w-md"
  >
    <div class="flex items-stretch gap-3 p-2">
      <img
        v-if="preview.image?.src"
        :src="preview.image.src"
        :alt="preview.title ?? ''"
        referrerpolicy="no-referrer"
        loading="lazy"
        decoding="async"
        class="w-20 h-20 object-cover rounded-lg flex-shrink-0 bg-muted"
      />
      <div class="flex-1 min-w-0 py-0.5">
        <div
          v-if="preview.siteName"
          class="text-[10px] font-bold uppercase tracking-wider text-muted-foreground/70 truncate"
        >{{ preview.siteName }}</div>
        <div class="text-[13px] font-semibold text-foreground truncate">{{ preview.title }}</div>
        <div
          v-if="preview.description"
          class="text-[12px] text-muted-foreground line-clamp-2 mt-0.5"
        >{{ preview.description }}</div>
      </div>
    </div>
  </a>
</template>
