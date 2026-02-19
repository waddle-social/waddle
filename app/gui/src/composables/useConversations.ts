import { ref, readonly } from 'vue';
import { defineStore } from 'pinia';
import {
  type ChatMessage,
  type RosterItem,
  type UnlistenFn,
  useWaddle,
} from './useWaddle';
import { useAuthStore } from '../stores/auth';

export interface ConversationSummary {
  jid: string;
  title: string;
  preview: string;
  updatedAt: string | null;
}

function conversationTitle(item: RosterItem): string {
  return item.name?.trim() || item.jid;
}

function bareJid(value: string): string {
  return value.split('/')[0] || value;
}

function titleFromJid(jid: string): string {
  return jid.split('@')[0] || jid;
}

interface ConversationMessageEventEnvelope {
  data?: {
    message?: ChatMessage;
  };
}

interface ConversationTransportEvent {
  payload?: ConversationMessageEventEnvelope;
  data?: {
    message?: ChatMessage;
  };
}

export const useConversationsStore = defineStore('conversations', () => {
  const { getHistory, getRoster, listen } = useWaddle();

  const conversations = ref<ConversationSummary[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  const unlistenFns: UnlistenFn[] = [];
  let listening = false;

  function sortSummaries(items: ConversationSummary[]): ConversationSummary[] {
    return items.sort((left, right) => {
      const leftTime = left.updatedAt ? Date.parse(left.updatedAt) : 0;
      const rightTime = right.updatedAt ? Date.parse(right.updatedAt) : 0;

      if (rightTime !== leftTime) {
        return rightTime - leftTime;
      }

      return left.title.localeCompare(right.title);
    });
  }

  function upsertConversation(summary: ConversationSummary): void {
    const current = [...conversations.value];
    const existingIdx = current.findIndex((item) => item.jid === summary.jid);

    if (existingIdx >= 0) {
      const existing = current[existingIdx];
      current[existingIdx] = {
        ...existing,
        title: summary.title || existing.title,
        preview: summary.preview || existing.preview,
        updatedAt: summary.updatedAt ?? existing.updatedAt,
      };
    } else {
      current.push(summary);
    }

    conversations.value = sortSummaries(current);
  }

  function summaryFromMessage(message: ChatMessage): ConversationSummary | null {
    if (message.messageType === 'groupchat') return null;

    const auth = useAuthStore();
    const self = bareJid(auth.jid);
    const from = bareJid(message.from);
    const to = bareJid(message.to);
    const peerJid = from === self ? to : from;

    if (!peerJid || peerJid === self) return null;

    const existing = conversations.value.find((item) => item.jid === peerJid);
    return {
      jid: peerJid,
      title: existing?.title || titleFromJid(peerJid),
      preview: message.body?.trim() || existing?.preview || '',
      updatedAt: message.timestamp ?? existing?.updatedAt ?? null,
    };
  }

  function extractMessageFromEvent(payload: unknown): ChatMessage | null {
    const eventPayload = payload as ConversationTransportEvent;
    const envelope = eventPayload.payload ?? eventPayload;
    return envelope.data?.message ?? null;
  }

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
      const merged = new Map<string, ConversationSummary>();
      for (const summary of summaries) merged.set(summary.jid, summary);
      for (const existing of conversations.value) {
        if (!merged.has(existing.jid)) {
          merged.set(existing.jid, existing);
        }
      }

      conversations.value = sortSummaries(Array.from(merged.values()));
    } catch (cause) {
      // Tolerate disconnected state — don't overwrite existing conversations
      if (conversations.value.length === 0) {
        error.value = cause instanceof Error ? cause.message : String(cause);
      }
    } finally {
      loading.value = false;
    }
  }

  const rosterEvents = [
    'xmpp.roster.received',
    'xmpp.roster.updated',
    'xmpp.roster.removed',
    'system.connection.established',
  ];

  function startListening(): void {
    if (listening) return;
    listening = true;

    void refreshConversations();

    for (const channel of rosterEvents) {
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

    for (const channel of ['xmpp.message.received', 'xmpp.message.sent']) {
      void listen<unknown>(channel, ({ payload }) => {
        const message = extractMessageFromEvent(payload);
        if (!message) {
          void refreshConversations();
          return;
        }
        const summary = summaryFromMessage(message);
        if (!summary) return;
        upsertConversation(summary);
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
