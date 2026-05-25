<script setup lang="ts">
import { useStore } from "@nanostores/vue";
import { MessageCircle, PhoneCall, Plus } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { formatTimelineStamp } from "@/channels/timeline";
import { $dmCallActivities } from "@/lib/calls/dm-call-activity";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { DmConversation } from "@/lib/xmpp-client";

const props = defineProps<{
  conversations: DmConversation[];
  activePeerJid: string | null;
}>();

const emit = defineEmits<{
  selectDm: [peerJid: string];
  newDm: [];
}>();

const dmCallActivities = useStore($dmCallActivities);

function preview(text?: string): string {
  if (!text) return "";
  return text.length > 44 ? `${text.slice(0, 44)}…` : text;
}

function dotClass(show?: DmConversation["presenceShow"]): string {
  if (show === "away") return "bg-warning/75";
  if (show === "dnd") return "bg-destructive/75";
  if (show === "xa") return "bg-warning/55";
  if (show === "available") return "bg-success/75";
  return "bg-muted-foreground/25";
}

function hasCallActivity(peerJid: string): boolean {
  const normalized = barePeerJid(peerJid).toLowerCase();
  return !!normalized && !!dmCallActivities.value[normalized];
}

function callActivityLabel(peerJid: string): string {
  const normalized = barePeerJid(peerJid).toLowerCase();
  const activity = normalized ? dmCallActivities.value[normalized] : null;
  return activity?.state === "ringing" ? "Ringing" : "Live";
}
</script>

<template>
  <div class="chat-sidebar-pane glass-panel">
    <div class="chat-sidebar-header">
      <div class="flex items-center gap-2">
        <MessageCircle class="w-4 h-4 text-primary/70" />
        <h2 class="type-pane-title text-sidebar-foreground">Direct messages</h2>
      </div>
      <button
        class="chat-icon-button text-sidebar-muted hover:bg-sidebar-accent hover:text-sidebar-foreground"
        title="New message"
        aria-label="New direct message"
        type="button"
        @click="emit('newDm')"
      >
        <Plus class="w-4 h-4" />
      </button>
    </div>

    <div class="chat-pane-scroll chat-sidebar-scroll">
      <!-- Empty state — halo + MessageCircle glyph + caption + hint
           matches the iter-37 / 47 / 48 authored-empty-state pattern
           used elsewhere in the app. -->
      <div v-if="conversations.length === 0" class="flex flex-col items-center justify-center gap-2 py-10 text-center">
        <div class="chat-empty-state__halo">
          <span class="chat-empty-state__halo-glow chat-empty-state__halo-glow--primary" aria-hidden="true" />
          <span class="chat-empty-state__halo-ring chat-empty-state__halo-ring--primary">
            <MessageCircle class="w-4 h-4 text-primary" aria-hidden="true" />
          </span>
        </div>
        <div class="type-control text-sidebar-foreground">No conversations yet</div>
        <div class="type-caption text-sidebar-muted max-w-[14rem]">
          Start one with the + button above.
        </div>
      </div>

      <div v-else class="chat-list-stack">
        <button
          v-for="conversation in conversations"
          :key="conversation.peerJid"
          class="chat-list-row w-full min-h-14 flex items-center gap-3 px-3 py-2 text-left group"
          :class="activePeerJid === conversation.peerJid
            ? 'chat-list-row--active bg-sidebar-accent text-sidebar-foreground'
            : 'text-sidebar-muted hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'"
          :aria-current="activePeerJid === conversation.peerJid ? 'page' : undefined"
          type="button"
          @click="emit('selectDm', conversation.peerJid)"
        >
          <div class="relative">
            <AppAvatar :name="conversation.peerUsername" :src="conversation.peerAvatarUrl ?? null" size="sm" />
            <span class="absolute -right-0.5 -bottom-0.5 w-2 h-2 rounded-full border border-background" :class="dotClass(conversation.presenceShow)" />
          </div>
          <div class="min-w-0 flex-1">
            <div class="flex items-center justify-between gap-2">
              <span class="type-control truncate">{{ conversation.peerUsername }}</span>
              <span v-if="conversation.lastMessageAt" class="type-meta type-numeric text-sidebar-muted">
                {{ formatTimelineStamp(conversation.lastMessageAt) }}
              </span>
            </div>
            <div class="flex items-center gap-1.5">
              <span class="type-caption text-sidebar-muted min-w-0 flex-1 truncate">{{ preview(conversation.lastMessageBody) }}</span>
              <span
                v-if="hasCallActivity(conversation.peerJid)"
                class="type-meta inline-flex h-[18px] shrink-0 items-center gap-1 rounded-full border border-success/25 bg-success/10 px-1.5 text-success"
                :title="callActivityLabel(conversation.peerJid)"
                :aria-label="callActivityLabel(conversation.peerJid)"
              >
                <PhoneCall class="h-3 w-3" />
                <span>{{ callActivityLabel(conversation.peerJid) }}</span>
              </span>
              <span
                v-if="conversation.unreadCount > 0"
                class="chat-badge-glow--primary type-count-badge inline-flex min-w-[18px] h-[18px] shrink-0 items-center justify-center rounded-full bg-primary px-1 text-primary-foreground"
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
