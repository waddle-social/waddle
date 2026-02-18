import { ref, readonly } from 'vue';
import { defineStore } from 'pinia';
import {
  type ChatMessage,
  type RosterItem,
  type UnlistenFn,
  useWaddle,
} from './useWaddle';

export interface ConversationSummary {
  jid: string;
  title: string;
  preview: string;
  updatedAt: string | null;
}

function conversationTitle(item: RosterItem): string {
  return item.name?.trim() || item.jid;
}

export const useConversationsStore = defineStore('conversations', () => {
  const { getHistory, getRoster, listen } = useWaddle();

  const conversations = ref<ConversationSummary[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  const unlistenFns: UnlistenFn[] = [];
  let listening = false;

  async function summarizeConversation(item: RosterItem): Promise<ConversationSummary> {
    let latestMessage: ChatMessage | undefined;
    try {
      const history = await getHistory(item.jid, 1);
      latestMessage = history[0];
    } catch {
      latestMessage = undefined;
    }

    return {
      jid: item.jid,
      title: conversationTitle(item),
      preview: latestMessage?.body?.trim() || '',
      updatedAt: latestMessage?.timestamp ?? null,
    };
  }

  async function refreshConversations(): Promise<void> {
    loading.value = true;
    error.value = null;

    try {
      const roster = await getRoster();
      const summaries = await Promise.all(roster.map((item) => summarizeConversation(item)));

      conversations.value = summaries.sort((left, right) => {
        const leftTime = left.updatedAt ? Date.parse(left.updatedAt) : 0;
        const rightTime = right.updatedAt ? Date.parse(right.updatedAt) : 0;

        if (rightTime !== leftTime) {
          return rightTime - leftTime;
        }

        return left.title.localeCompare(right.title);
      });
    } catch (cause) {
      // Tolerate disconnected state — don't overwrite existing conversations
      if (conversations.value.length === 0) {
        error.value = cause instanceof Error ? cause.message : String(cause);
      }
    } finally {
      loading.value = false;
    }
  }

  const conversationEvents = [
    'xmpp.roster.received',
    'xmpp.roster.updated',
    'xmpp.roster.removed',
    'xmpp.message.received',
    'xmpp.message.sent',
    'system.connection.established',
  ];

  function startListening(): void {
    if (listening) return;
    listening = true;

    void refreshConversations();

    for (const channel of conversationEvents) {
      void listen(channel, () => {
        void refreshConversations();
      })
        .then((unlisten) => {
          unlistenFns.push(unlisten);
        })
        .catch(() => {
          // Transport not ready — tolerate gracefully
        });
    }
  }

  function stopListening(): void {
    while (unlistenFns.length > 0) {
      const unlisten = unlistenFns.pop();
      unlisten?.();
    }
    listening = false;
  }

  return {
    conversations: readonly(conversations),
    loading: readonly(loading),
    error: readonly(error),
    refreshConversations,
    startListening,
    stopListening,
  };
});

/** Convenience alias matching the original composable API */
export function useConversations() {
  return useConversationsStore();
}
