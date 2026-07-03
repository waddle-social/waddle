<script setup lang="ts">
import AppAvatar from "@/components/ui/AppAvatar.vue";
import type { OccupantPresence } from "@/lib/xmpp-client";

defineProps<{
  typingUsers: string[];
  avatarUrlByAuthor: Record<string, string | null>;
  roomPresence: Record<string, OccupantPresence>;
  /** social = top-pinned feed (bordered), chat = classic bottom composer. */
  variant: "social" | "chat";
}>();
</script>

<template>
  <div
    class="chat-typing-indicator flex-shrink-0"
    :class="variant === 'social'
      ? 'chat-typing-indicator--social border-b border-border'
      : 'chat-typing-indicator--chat'"
  >
    <div class="chat-message-lane chat-typing-indicator__lane">
      <span class="chat-typing-indicator__avatars" aria-hidden="true">
        <span
          v-for="nick in typingUsers.slice(0, 3)"
          :key="`typing-avatar:${variant}:${nick}`"
          class="chat-typing-indicator__avatar-wrap"
        >
          <AppAvatar
            :name="nick"
            :src="avatarUrlByAuthor[nick] ?? null"
            :presence="roomPresence[nick] ?? 'offline'"
            size="xs"
          />
        </span>
      </span>
      <span class="chat-typing-indicator__bubble">
        <span class="chat-typing-indicator__wave" aria-hidden="true">
          <span class="chat-typing-indicator__wave-dot" />
          <span class="chat-typing-indicator__wave-dot" />
          <span class="chat-typing-indicator__wave-dot" />
        </span>
        <span class="chat-typing-indicator__text">
          <template v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing</template>
          <template v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing</template>
          <template v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing</template>
        </span>
      </span>
    </div>
  </div>
</template>
