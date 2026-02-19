import { ref, readonly } from 'vue';
import { defineStore } from 'pinia';
import {
  type ChatMessage,
  type RosterItem,
  type UnlistenFn,
  useWaddle,
} from './useWaddle';
import { useAuthStore } from '../stores/auth';
import { useSettingsStore } from '../stores/settings';
import {
  buildConversationUpdateFromMessage,
  clearConversationUnreadCount,
  type ConversationSummaryLike,
  normalizeConversationJid,
  peerJidForDirectMessage,
} from '../utils/conversationState';
import { extractMessageFromEventPayload } from '../utils/eventPayload';

export interface ConversationSummary extends ConversationSummaryLike {}

function conversationTitle(item: RosterItem): string {
  return item.name?.trim() || item.jid;
}

function titleFromJid(jid: string): string {
  return jid.split('@')[0] || jid;
}

function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export const useConversationsStore = defineStore('conversations', () => {
  const { getHistory, getRoster, listen } = useWaddle();
  const authStore = useAuthStore();
  const settingsStore = useSettingsStore();

  const conversations = ref<ConversationSummary[]>([]);
  const activeConversationJid = ref<string | null>(null);
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
      if (!existing) return;
      current[existingIdx] = {
        ...existing,
        title: summary.title || existing.title,
        preview: summary.preview || existing.preview,
        updatedAt: summary.updatedAt ?? existing.updatedAt,
        unreadCount: summary.unreadCount ?? existing.unreadCount ?? 0,
      };
    } else {
      current.push({
        ...summary,
        unreadCount: summary.unreadCount ?? 0,
      });
    }

    conversations.value = sortSummaries(current);
  }

  async function maybeNotifyIncomingDm(
    message: ChatMessage,
    summary: ConversationSummary,
    isIncoming: boolean,
  ): Promise<void> {
    if (!isIncoming || isTauriRuntime()) return;
    if (!settingsStore.notificationsEnabled) return;
    if (activeConversationJid.value === summary.jid) return;
    if (typeof window === 'undefined' || typeof Notification === 'undefined') return;

    let permission = Notification.permission;
    if (permission === 'default') {
      try {
        permission = await Notification.requestPermission();
      } catch {
        return;
      }
    }
    if (permission !== 'granted') return;

    const body = message.body.trim() || 'New message';
    try {
      new Notification(summary.title || titleFromJid(summary.jid), {
        body,
        tag: `dm:${summary.jid}`,
      });
    } catch {
      // Notification API can fail in restricted environments.
    }
  }

  function setActiveConversation(jid: string | null): void {
    activeConversationJid.value = normalizeConversationJid(jid);
    if (!activeConversationJid.value) return;

    conversations.value = sortSummaries(
      clearConversationUnreadCount(conversations.value, activeConversationJid.value),
    );
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
      unreadCount: 0,
    };
  }

  async function refreshConversations(): Promise<void> {
    loading.value = true;
    error.value = null;

    try {
      const roster = await getRoster();
      const summaries = await Promise.all(roster.map((item) => summarizeConversation(item)));
      const existingByJid = new Map<string, ConversationSummary>(
        conversations.value.map((item) => [item.jid, item]),
      );
      const merged = new Map<string, ConversationSummary>();
      for (const summary of summaries) {
        const existing = existingByJid.get(summary.jid);
        merged.set(summary.jid, {
          ...summary,
          unreadCount: existing?.unreadCount ?? 0,
        });
      }
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
        const message = extractMessageFromEventPayload<ChatMessage>(payload);
        if (!message) {
          void refreshConversations();
          return;
        }
        const peerJid = peerJidForDirectMessage(message, authStore.jid);
        if (!peerJid) return;
        const existing = conversations.value.find((item) => item.jid === peerJid) ?? null;
        const update = buildConversationUpdateFromMessage<ConversationSummary>(message, {
          selfJid: authStore.jid,
          activeConversationJid: activeConversationJid.value,
          existing,
          titleFromJid,
        });
        if (!update) return;

        upsertConversation(update.summary);
        void maybeNotifyIncomingDm(message, update.summary, update.isIncoming);
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
    activeConversationJid: readonly(activeConversationJid),
    loading: readonly(loading),
    error: readonly(error),
    setActiveConversation,
    refreshConversations,
    startListening,
    stopListening,
  };
});

/** Convenience alias matching the original composable API */
export function useConversations() {
  return useConversationsStore();
}
