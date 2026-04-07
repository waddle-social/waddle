<script setup lang="ts">
import { computed } from "vue";
import { Plus, Search, Lock } from "lucide-vue-next";
import type { WaddleSummary } from "@/lib/waddle-api";

const props = defineProps<{
  waddles: WaddleSummary[];
  activeWaddleId: string | null;
}>();

const emit = defineEmits<{
  selectWaddle: [id: string];
  createWaddle: [];
}>();

const publicWaddles = computed(() =>
  props.waddles.filter((w) => w.is_public),
);

const privateWaddles = computed(() =>
  props.waddles.filter((w) => !w.is_public),
);

function waddleColor(waddle: WaddleSummary): string {
  const colors = [
    "#E63946", "#457B9D", "#2A9D8F", "#F77F00",
    "#06FFA5", "#9D4EDD", "#FF006E", "#8338EC",
    "#FB5607", "#3A86FF",
  ];
  let hash = 0;
  for (const char of waddle.id) hash = ((hash << 5) - hash + char.charCodeAt(0)) | 0;
  return colors[Math.abs(hash) % colors.length];
}
</script>

<template>
  <div class="w-48 border-r border-foreground bg-background flex flex-col flex-shrink-0">
    <div class="h-20 border-b border-foreground px-6 flex items-center justify-between">
      <span class="text-sm font-mono font-bold uppercase tracking-wider">Waddles</span>
      <div class="flex gap-1">
        <button
          class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
          @click="emit('createWaddle')"
        >
          <Plus class="w-3.5 h-3.5" />
        </button>
      </div>
    </div>

    <div class="flex-1 overflow-auto p-4">
      <div v-if="privateWaddles.length > 0" class="mb-6">
        <div class="px-2 mb-3">
          <span class="text-[10px] font-mono uppercase tracking-widest text-muted-foreground/60">
            Private
          </span>
        </div>
        <div class="space-y-1">
          <button
            v-for="waddle in privateWaddles"
            :key="waddle.id"
            class="w-full flex items-center gap-2 px-2 py-2 transition-colors"
            :class="activeWaddleId === waddle.id ? 'bg-foreground text-background' : 'hover:bg-muted'"
            @click="emit('selectWaddle', waddle.id)"
          >
            <div class="w-1 h-6" :style="{ backgroundColor: waddleColor(waddle) }" />
            <span class="text-sm font-mono uppercase tracking-wider truncate flex-1 text-left">
              {{ waddle.name }}
            </span>
            <Lock class="w-3 h-3 opacity-60" />
          </button>
        </div>
      </div>

      <div v-if="publicWaddles.length > 0">
        <div class="px-2 mb-3">
          <span class="text-[10px] font-mono uppercase tracking-widest text-muted-foreground/60">
            Communities
          </span>
        </div>
        <div class="space-y-1">
          <button
            v-for="waddle in publicWaddles"
            :key="waddle.id"
            class="w-full flex items-center gap-2 px-2 py-2 transition-colors"
            :class="activeWaddleId === waddle.id ? 'bg-foreground text-background' : 'hover:bg-muted'"
            @click="emit('selectWaddle', waddle.id)"
          >
            <div class="w-1 h-6" :style="{ backgroundColor: waddleColor(waddle) }" />
            <span class="text-sm font-mono uppercase tracking-wider truncate flex-1 text-left">
              {{ waddle.name }}
            </span>
          </button>
        </div>
      </div>

      <div
        v-if="waddles.length === 0"
        class="text-center py-8 text-sm font-mono text-muted-foreground"
      >
        No waddles yet
      </div>
    </div>
  </div>
</template>
