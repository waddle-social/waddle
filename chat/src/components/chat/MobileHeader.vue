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
  <div class="lg:hidden sticky top-0 z-10 grid grid-cols-[auto_1fr_auto] gap-2 items-center px-3 py-2.5 border-b border-border glass-panel">
    <button
      class="h-9 w-9 flex items-center justify-center rounded-xl hover:bg-muted transition-all duration-200"
      @click="emit('openNav')"
    >
      <Menu class="w-4 h-4" />
    </button>
    <div class="min-w-0 text-center">
      <div class="font-display font-bold text-[14px] truncate flex items-center justify-center gap-1.5">
        <Settings v-if="page === 'settings'" class="w-3 h-3 text-primary/60" />
        <Hash v-else-if="channel && !dmPeer" class="w-3 h-3 text-primary/60" />
        <MessageCircle v-else-if="dmPeer" class="w-3 h-3 text-primary/60" />
        {{ page === "settings" ? "Settings" : (dmPeer ? dmPeer.peerUsername : (channel ? channel.name : waddle?.name ?? "Waddle")) }}
      </div>
      <div v-if="page === 'settings'" class="text-[11px] text-muted-foreground truncate">
        Personal preferences
      </div>
      <div v-else-if="channel && waddle && !dmPeer" class="text-[11px] text-muted-foreground truncate">
        {{ waddle.name }}
      </div>
      <div v-else-if="dmPeer" class="text-[11px] text-muted-foreground truncate">
        Direct Message
      </div>
    </div>
    <button
      v-if="page !== 'settings'"
      class="h-9 w-9 flex items-center justify-center rounded-xl hover:bg-muted transition-all duration-200"
      @click="emit('openDetails')"
    >
      <Info class="w-4 h-4" />
    </button>
  </div>
</template>
