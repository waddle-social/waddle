<script setup lang="ts">
import { Menu, Info, Hash } from "lucide-vue-next";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";

defineProps<{
  waddle: WaddleSummary | null;
  channel: ChannelSummary | null;
  session: WaddleSession | null;
}>();

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
}>();
</script>

<template>
  <div class="lg:hidden sticky top-0 z-10 grid grid-cols-[auto_1fr_auto] gap-2 items-center px-3 py-2.5 border-b border-border glass-panel">
    <button
      class="h-9 w-9 flex items-center justify-center rounded-xl hover:bg-muted transition-all duration-200"
      @click="emit('openNav')"
    >
      <Menu class="w-4 h-4" />
    </button>
    <div class="min-w-0 text-center">
      <div class="font-display font-bold text-[14px] truncate flex items-center justify-center gap-1.5">
        <Hash v-if="channel" class="w-3 h-3 text-primary/60" />
        {{ channel ? channel.name : waddle?.name ?? "Waddle" }}
      </div>
      <div v-if="channel && waddle" class="text-[11px] text-muted-foreground truncate">
        {{ waddle.name }}
      </div>
    </div>
    <button
      class="h-9 w-9 flex items-center justify-center rounded-xl hover:bg-muted transition-all duration-200"
      @click="emit('openDetails')"
    >
      <Info class="w-4 h-4" />
    </button>
  </div>
</template>
