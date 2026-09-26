<script setup lang="ts">
import { Hash, LockKeyhole, MessageCircle, MessagesSquare } from "lucide-vue-next";

defineProps<{
  /**
   * pick  → no conversation selected yet.
   * quiet → conversation selected but its timeline is empty.
   * access-required → the server denied this account access to the channel.
   */
  variant: "pick" | "quiet" | "access-required";
  sidebarMode?: "channels" | "dms";
  isForumChannel: boolean;
  dmPeerUsername?: string | null;
  channelName?: string | null;
}>();
</script>

<template>
  <div
    v-if="variant === 'access-required'"
    class="chat-empty-state"
    role="status"
    aria-live="polite"
  >
    <span class="flex h-14 w-14 items-center justify-center rounded-2xl border border-border bg-card text-muted-foreground" aria-hidden="true">
      <LockKeyhole class="h-6 w-6" aria-hidden="true" />
    </span>
    <div class="chat-field-stack">
      <p class="font-display text-lg font-semibold tracking-[-0.01em] text-foreground">
        You need access to this channel
      </p>
      <p class="chat-copy-measure text-sm text-muted-foreground">
        Ask a space admin for access, then open the channel again to retry.
      </p>
    </div>
  </div>

  <div v-else-if="variant === 'pick'" class="chat-empty-state">
    <span class="flex h-14 w-14 items-center justify-center rounded-2xl border border-border bg-card text-primary" aria-hidden="true">
      <component :is="sidebarMode === 'dms' ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="h-6 w-6" aria-hidden="true" />
    </span>
    <div class="chat-field-stack">
      <p class="font-display text-lg font-semibold tracking-[-0.01em] text-foreground">
        {{ sidebarMode === "dms"
          ? "Pick a conversation"
          : isForumChannel
            ? "Pick a forum"
            : "Pick a channel" }}
      </p>
      <p class="chat-copy-measure text-sm text-muted-foreground">
        {{ sidebarMode === "dms"
          ? "Open one from the sidebar to keep chatting."
          : isForumChannel
            ? "Open one to browse its topics."
            : "Open one from the sidebar to drop into the conversation." }}
      </p>
    </div>
  </div>

  <div v-else class="chat-empty-state">
    <div v-if="!dmPeerUsername && !isForumChannel" class="flex h-28 w-28 items-center justify-center" aria-hidden="true">
      <img class="h-full w-full" src="/waddle-logo.svg" alt="" />
    </div>
    <span v-else class="flex h-14 w-14 items-center justify-center rounded-2xl border border-border bg-card text-primary" aria-hidden="true">
      <component :is="dmPeerUsername ? MessageCircle : MessagesSquare" class="h-6 w-6" aria-hidden="true" />
    </span>
    <div class="chat-field-stack">
      <p class="font-display text-lg font-semibold tracking-[-0.01em] text-foreground">
        {{ dmPeerUsername
          ? `Just you and @${dmPeerUsername}`
          : isForumChannel
            ? `Welcome to #${channelName}`
            : `It's quiet in #${channelName}` }}
      </p>
      <p class="chat-copy-measure text-sm text-muted-foreground">
        {{ isForumChannel
          ? "Start the first topic with a clear title so people can follow the thread."
          : dmPeerUsername
            ? "Send the first message to get the conversation going."
            : "Be the one who breaks the silence. Drop a hello, share what you're working on, ask the room something interesting." }}
      </p>
    </div>
  </div>
</template>
