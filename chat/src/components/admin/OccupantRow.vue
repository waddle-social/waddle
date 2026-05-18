<script setup lang="ts">
// Admin V2 — single occupant row used by ChannelDetailDrawer. Shows
// nick + real JID + role/affiliation chips and a "Kick" button.
import { UserMinus } from "lucide-vue-next";
import type { WasmAdminChannelOccupantEntry } from "@/lib/xmpp";

defineProps<{
  entry: WasmAdminChannelOccupantEntry;
  kicking?: boolean;
}>();

const emit = defineEmits<{
  kick: [];
}>();
</script>

<template>
  <div class="flex items-center gap-2 rounded-md border border-border bg-card px-2.5 py-2">
    <div class="flex flex-col gap-0.5 min-w-0 flex-1">
      <span class="type-control truncate">{{ entry.nick }}</span>
      <span class="font-mono type-caption text-muted-foreground truncate">{{ entry.real_jid }}</span>
      <div class="flex flex-wrap items-center gap-1">
        <span class="type-caption rounded-full bg-muted px-2 py-0.5">{{ entry.role }}</span>
        <span class="type-caption rounded-full bg-muted px-2 py-0.5">{{ entry.affiliation }}</span>
      </div>
    </div>
    <button
      type="button"
      class="chat-action-button chat-action-button--destructive type-control"
      :disabled="kicking"
      :aria-label="`Kick ${entry.nick}`"
      @click="emit('kick')"
    >
      <UserMinus class="w-4 h-4" />
      <span class="hidden sm:inline">{{ kicking ? "Kicking…" : "Kick" }}</span>
    </button>
  </div>
</template>
