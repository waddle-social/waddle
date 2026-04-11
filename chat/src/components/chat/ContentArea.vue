<script setup lang="ts">
import { watch } from "vue";
import { Hash, Settings } from "lucide-vue-next";
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { XmppStatusSnapshot } from "@/lib/xmpp-client";
import MessageCard from "@/components/chat/MessageCard.vue";
import MessageComposer from "@/components/chat/MessageComposer.vue";

const draft = defineModel<string>("draft", { required: true });

const props = defineProps<{
  waddle: WaddleSummary | null;
  channel: ChannelSummary | null;
  messages: TimelineMessage[];
  xmppStatus: XmppStatusSnapshot;
  actionError: string;
  isLoadingMessages: boolean;
  isSending: boolean;
  canManageChannels: boolean;
  typingUsers: string[];
  currentUser?: string;
  tenorApiKey: string;
  memberNames: string[];
}>();

const emit = defineEmits<{
  send: [];
  typing: [];
  selectGif: [url: string];
  editMessage: [messageId: string, newBody: string];
  retractMessage: [messageId: string];
  reactMessage: [messageId: string, emoji: string];
  displayed: [messageId: string];
  editChannel: [];
}>();

// XEP-0333: Send displayed marker for the latest non-self message
watch(
  () => props.messages,
  (msgs) => {
    const last = [...msgs].reverse().find((m) => !m.isSelf && !m.isRetracted);
    if (last) {
      emit("displayed", last.id);
    }
  },
  { deep: true },
);
</script>

<template>
  <div class="flex-1 flex flex-col min-w-0">
    <!-- Header -->
    <div class="h-20 border-b border-foreground px-6 flex items-center justify-between bg-background flex-shrink-0">
      <div class="flex items-center gap-4">
        <div class="flex items-center gap-2">
          <Hash class="w-4 h-4" />
          <h1 class="text-xl font-mono font-bold">{{ channel?.name ?? "..." }}</h1>
        </div>
        <div
          v-if="xmppStatus.state !== 'online'"
          class="flex items-center gap-2 text-xs font-mono text-muted-foreground"
        >
          <span
            class="w-2 h-2 inline-block"
            :class="xmppStatus.state === 'error' ? 'bg-destructive' : 'bg-muted-foreground'"
          />
          {{ xmppStatus.state }}
        </div>
      </div>
      <button
        v-if="canManageChannels && channel"
        class="h-7 w-7 flex items-center justify-center hover:bg-muted transition-colors"
        title="Channel settings"
        @click="emit('editChannel')"
      >
        <Settings class="w-3.5 h-3.5" />
      </button>
    </div>

    <!-- Error banner -->
    <div
      v-if="actionError"
      class="px-6 py-3 bg-destructive/10 border-b border-destructive/20 text-sm font-mono text-destructive"
    >
      {{ actionError }}
    </div>

    <!-- Messages -->
    <div class="flex-1 overflow-auto px-6 py-6">
      <div v-if="isLoadingMessages" class="text-center py-8 text-sm font-mono text-muted-foreground">
        Loading messages...
      </div>

      <div v-else-if="!channel" class="text-center py-8 text-sm font-mono text-muted-foreground">
        Select a channel to start chatting
      </div>

      <div v-else-if="messages.length === 0" class="text-center py-8 text-sm font-mono text-muted-foreground">
        No messages yet. Start the conversation!
      </div>

      <div v-else class="space-y-4 max-w-4xl">
        <MessageCard
          v-for="msg in messages"
          :key="msg.id"
          :message="msg"
          :current-user="props.currentUser"
          @edit="(id, body) => emit('editMessage', id, body)"
          @retract="(id) => emit('retractMessage', id)"
          @react="(id, emoji) => emit('reactMessage', id, emoji)"
        />
      </div>
    </div>

    <!-- Typing indicator -->
    <div
      v-if="typingUsers.length > 0"
      class="px-6 py-1.5 text-xs font-mono text-muted-foreground flex-shrink-0"
    >
      <span v-if="typingUsers.length === 1">{{ typingUsers[0] }} is typing...</span>
      <span v-else-if="typingUsers.length === 2">{{ typingUsers[0] }} and {{ typingUsers[1] }} are typing...</span>
      <span v-else>{{ typingUsers[0] }} and {{ typingUsers.length - 1 }} others are typing...</span>
    </div>

    <!-- Composer -->
    <MessageComposer
      v-if="channel"
      v-model:draft="draft"
      :channel-name="channel.name"
      :is-sending="isSending"
      :disabled="!channel"
      :tenor-api-key="tenorApiKey"
      :member-names="memberNames"
      @send="emit('send')"
      @typing="emit('typing')"
      @select-gif="(url) => emit('selectGif', url)"
    />
  </div>
</template>
