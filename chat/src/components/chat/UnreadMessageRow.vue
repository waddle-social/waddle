<script setup lang="ts">
import type { TimelineMessage } from "@/lib/chat-ui";
import { formatTimelineStamp } from "@/channels/timeline";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import MessageBody from "@/components/chat/MessageBody.vue";

// Read-only message row for the Unread digest. Reuses `MessageBody`
// (the same presentational component `PinnedPanel` uses) so unread
// messages render their full bodies — markup, mentions, attachments,
// link previews — without any of the live-channel action wiring that
// `MessageCard` requires.
defineProps<{
  message: TimelineMessage;
}>();
</script>

<template>
  <div class="flex gap-2.5 px-2 py-2">
    <AppAvatar :name="message.author" size="sm" class="mt-0.5 flex-shrink-0" />
    <div class="min-w-0 flex-1">
      <div class="flex items-baseline justify-between gap-2">
        <span class="type-field font-medium truncate">{{ message.author }}</span>
        <span class="type-field-xs text-muted-foreground shrink-0">
          {{ formatTimelineStamp(message.createdAt) }}
        </span>
      </div>
      <p
        v-if="message.isRetracted"
        class="type-field-sm italic text-muted-foreground"
      >Message retracted</p>
      <MessageBody v-else :message="message" compact />
    </div>
  </div>
</template>
