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
  <div class="lg:hidden sticky top-0 z-10 grid grid-cols-[auto_1fr_auto] gap-2 items-center px-3 py-2 border-b border-border bg-background/95 backdrop-blur-sm">
    <button
      class="h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted transition-colors"
      @click="emit('openNav')"
    >
      <Menu class="w-4 h-4" />
    </button>
    <div class="min-w-0 text-center">
      <div class="font-medium text-[13px] truncate flex items-center justify-center gap-1">
        <Hash v-if="channel" class="w-3 h-3 text-muted-foreground" />
        {{ channel ? channel.name : waddle?.name ?? "Waddle" }}
      </div>
      <div v-if="channel && waddle" class="text-[11px] text-muted-foreground truncate">
        {{ waddle.name }}
      </div>
    </div>
    <button
      class="h-8 w-8 flex items-center justify-center rounded-md hover:bg-muted transition-colors"
      @click="emit('openDetails')"
    >
      <Info class="w-4 h-4" />
    </button>
  </div>
</template>
