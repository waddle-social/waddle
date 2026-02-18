import { computed, ref } from 'vue';
import { defineStore } from 'pinia';

export interface ConversationSummary {
  jid: string;
  title: string;
  unreadCount: number;
  preview: string;
}

export interface ConversationMessage {
  id: string;
  from: string;
  body: string;
  sentAt: string;
}

export const useConversationsStore = defineStore('conversations', () => {
  const conversations = ref<ConversationSummary[]>([
    {
      jid: 'alice@example.com',
      title: 'Alice',
      unreadCount: 2,
      preview: 'Are we shipping this sprint?',
    },
    {
      jid: 'room@conference.example.com',
      title: 'Engineering Room',
      unreadCount: 0,
      preview: 'Daily standup notes were posted.',
    },
  ]);

  const messagesByJid = ref<Record<string, ConversationMessage[]>>({
    'alice@example.com': [
      {
        id: 'msg-001',
        from: 'alice@example.com',
        body: 'Are we shipping this sprint?',
        sentAt: '2026-02-11T16:00:00Z',
      },
    ],
    'room@conference.example.com': [
      {
        id: 'msg-002',
        from: 'charlie@example.com',
        body: 'Daily standup notes were posted.',
        sentAt: '2026-02-11T15:40:00Z',
      },
    ],
  });

  const orderedConversations = computed(() =>
    [...conversations.value].sort((left, right) => right.unreadCount - left.unreadCount),
  );

  function messagesFor(jid: string): ConversationMessage[] {
    return messagesByJid.value[jid] ?? [];
  }

  return {
    conversations,
    messagesByJid,
    orderedConversations,
    messagesFor,
  };
});
