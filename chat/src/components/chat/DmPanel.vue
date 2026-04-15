<script setup lang="ts">
import { MessageCircle, Plus } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { formatStamp } from "@/composables/useMessaging";
import type { DmConversation } from "@/lib/xmpp-client";

const props = defineProps<{
  conversations: DmConversation[];
  activePeerJid: string | null;
}>();

const emit = defineEmits<{
  selectDm: [peerJid: string];
  newDm: [];
}>();

function preview(text?: string): string {
  if (!text) return "";
  return text.length > 44 ? `${text.slice(0, 44)}…` : text;
}

function dotClass(show?: DmConversation["presenceShow"]): string {
  if (show === "away") return "bg-warning";
  if (show === "dnd") return "bg-destructive";
  if (show === "xa") return "bg-orange-500";
  if (show === "available") return "bg-success";
  return "bg-muted-foreground/40";
}
</script>

<template>
  <div class="w-[248px] border-r border-border glass-panel flex flex-col flex-shrink-0">
    <div class="h-14 px-4 flex items-center justify-between border-b border-border">
      <div class="flex items-center gap-2">
        <MessageCircle class="w-4 h-4 text-primary/70" />
        <h2 class="text-[14px] font-display font-bold tracking-tight text-sidebar-foreground">Direct Messages</h2>
      </div>
      <button
        class="h-7 w-7 flex items-center justify-center rounded-lg text-sidebar-muted hover:bg-sidebar-accent hover:text-sidebar-foreground transition-all duration-200"
        title="New message"
        @click="emit('newDm')"
      >
        <Plus class="w-4 h-4" />
      </button>
    </div>

    <div class="flex-1 overflow-auto py-3 px-2">
      <div v-if="conversations.length === 0" class="text-center py-8 text-[13px] text-sidebar-muted">
        No conversations yet
      </div>

      <div v-else class="space-y-0.5">
        <button
          v-for="conversation in conversations"
          :key="conversation.peerJid"
          class="w-full flex items-center gap-2.5 px-2.5 py-2 rounded-lg transition-all duration-200 text-left group"
          :class="activePeerJid === conversation.peerJid
            ? 'bg-sidebar-accent text-sidebar-foreground'
            : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
          @click="emit('selectDm', conversation.peerJid)"
        >
          <div class="relative">
            <AppAvatar :name="conversation.peerUsername" :src="conversation.peerAvatarUrl ?? null" size="sm" />
            <span class="absolute -right-0.5 -bottom-0.5 w-2.5 h-2.5 rounded-full border border-background" :class="dotClass(conversation.presenceShow)" />
          </div>
          <div class="min-w-0 flex-1">
            <div class="flex items-center justify-between gap-2">
              <span class="text-[13px] truncate font-medium">{{ conversation.peerUsername }}</span>
              <span v-if="conversation.lastMessageAt" class="text-[10px] font-mono text-sidebar-muted tabular-nums">
                {{ formatStamp(conversation.lastMessageAt) }}
              </span>
            </div>
            <div class="flex items-center gap-1.5">
              <span class="text-[11px] text-sidebar-muted truncate">{{ preview(conversation.lastMessageBody) }}</span>
              <span
                v-if="conversation.unreadCount > 0"
                class="ml-auto inline-flex min-w-[18px] h-[18px] px-1 items-center justify-center rounded-full text-[10px] font-semibold bg-primary text-primary-foreground"
              >
                {{ conversation.unreadCount }}
              </span>
            </div>
          </div>
        </button>
      </div>
    </div>
  </div>
</template>
