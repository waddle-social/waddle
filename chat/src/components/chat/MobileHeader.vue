<script setup lang="ts">
import { Menu, Info, Hash, MessageCircle, Settings } from "lucide-vue-next";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";

defineProps<{
  page?: "chat" | "settings";
  waddle: WaddleSummary | null;
  channel: ChannelSummary | null;
  dmPeer?: { peerUsername: string } | null;
  sidebarMode?: "channels" | "dms";
  session: WaddleSession | null;
}>();

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
}>();
</script>

<template>
  <div class="chat-mobile-header z-sticky lg:hidden sticky top-0 grid grid-cols-[auto_1fr_auto] items-center gap-3 border-b border-border px-3 py-1.5 glass-panel">
    <button
      class="h-11 w-11 flex items-center justify-center rounded-lg hover:bg-muted transition-all duration-200"
      type="button"
      aria-label="Open navigation"
      @click="emit('openNav')"
    >
      <Menu class="w-4 h-4" />
    </button>
    <div class="min-w-0 text-left">
      <div class="type-pane-title truncate flex items-center justify-start gap-1.5">
        <Settings v-if="page === 'settings'" class="w-3 h-3 text-primary/60" />
        <Hash v-else-if="channel && !dmPeer" class="w-3 h-3 text-primary/60" />
        <MessageCircle v-else-if="dmPeer" class="w-3 h-3 text-primary/60" />
        {{ page === "settings" ? "Settings" : (dmPeer ? dmPeer.peerUsername : (channel ? channel.name : waddle?.name ?? "Waddle")) }}
      </div>
      <div v-if="page === 'settings'" class="type-caption text-muted-foreground truncate">
        Personal preferences
      </div>
      <div v-else-if="channel && waddle && !dmPeer" class="type-caption text-muted-foreground truncate">
        {{ waddle.name }}
      </div>
      <div v-else-if="dmPeer" class="type-caption text-muted-foreground truncate">
        Direct message
      </div>
    </div>
    <button
      v-if="page !== 'settings'"
      class="h-11 w-11 flex items-center justify-center rounded-lg hover:bg-muted transition-all duration-200"
      type="button"
      aria-label="Open details"
      @click="emit('openDetails')"
    >
      <Info class="w-4 h-4" />
    </button>
  </div>
</template>
