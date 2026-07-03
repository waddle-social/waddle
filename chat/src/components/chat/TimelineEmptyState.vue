<script setup lang="ts">
import { Hash, MessageCircle, MessagesSquare } from "lucide-vue-next";

defineProps<{
  /**
   * pick  → no conversation selected yet.
   * quiet → conversation selected but its timeline is empty.
   */
  variant: "pick" | "quiet";
  sidebarMode?: "channels" | "dms";
  isForumChannel: boolean;
  dmPeerUsername?: string | null;
  channelName?: string | null;
}>();
</script>

<template>
  <div v-if="variant === 'pick'" class="chat-empty-state">
    <div class="chat-empty-state__halo">
      <span class="chat-empty-state__halo-glow" aria-hidden="true" />
      <span class="chat-empty-state__halo-ring chat-empty-state__halo-ring--muted">
        <component :is="sidebarMode === 'dms' ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-6 h-6 text-primary/70" />
      </span>
    </div>
    <div class="chat-field-stack">
      <p class="type-empty-title">
        {{ sidebarMode === "dms"
          ? "Pick a conversation"
          : isForumChannel
            ? "Pick a forum"
            : "Pick a channel" }}
      </p>
      <p class="type-field text-muted-foreground chat-copy-measure">
        {{ sidebarMode === "dms"
          ? "Open one from the sidebar to keep chatting."
          : isForumChannel
            ? "Open one to browse its topics."
            : "Open one from the sidebar to drop into the conversation." }}
      </p>
    </div>
  </div>

  <div v-else class="chat-empty-state">
    <div v-if="!dmPeerUsername && !isForumChannel" class="chat-empty-state__mascot-wrap" aria-hidden="true">
      <span class="chat-empty-state__mascot-halo" />
      <img class="chat-empty-state__mascot" src="/waddle-logo.svg" alt="" />
    </div>
    <div v-else class="chat-empty-state__halo">
      <span class="chat-empty-state__halo-glow chat-empty-state__halo-glow--primary" aria-hidden="true" />
      <span class="chat-empty-state__halo-ring chat-empty-state__halo-ring--primary">
        <component :is="dmPeerUsername ? MessageCircle : MessagesSquare" class="w-6 h-6 text-primary" />
      </span>
    </div>
    <div class="chat-field-stack">
      <p class="type-empty-title">
        {{ dmPeerUsername
          ? `Just you and @${dmPeerUsername}`
          : isForumChannel
            ? `Welcome to #${channelName}`
            : `It's quiet in #${channelName}` }}
      </p>
      <p class="type-field text-muted-foreground chat-copy-measure">
        {{ isForumChannel
          ? "Start the first topic with a clear title so people can follow the thread."
          : dmPeerUsername
            ? "Send the first message to get the conversation going."
            : "Be the one who breaks the silence. Drop a hello, share what you're working on, ask the room something interesting." }}
      </p>
    </div>
  </div>
</template>
