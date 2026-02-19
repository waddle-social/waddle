import { ref, readonly, watch } from 'vue';
import { useConversations } from './useConversations';
import { useWaddle, type ChatMessage, type UnlistenFn } from './useWaddle';
import { useRoomsStore } from '../stores/rooms';
import { storeToRefs } from 'pinia';

export interface FeedMessage extends ChatMessage {
  /** Used for deduplication */
  _feedKey: string;
}

/**
 * Composable that merges messages from all conversations and rooms
 * into a single chronological feed (most recent first).
 */
export function useFeed() {
  const conversationsStore = useConversations();
  const { conversations } = storeToRefs(conversationsStore);
  const { getHistory, listen } = useWaddle();
  const roomsStore = useRoomsStore();
  const { joinedRooms } = storeToRefs(roomsStore);

  const messages = ref<FeedMessage[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  const unlistenFns: UnlistenFn[] = [];

  /**
   * Checks if two messages are from the same sender within 5 minutes
   * (for grouping into compact view).
   */
  function shouldShowHeader(index: number): boolean {
    if (index === 0) return true;
    const prev = messages.value[index - 1];
    const curr = messages.value[index];
    if (!prev || !curr) return true;

    // Different sender
    const prevSender = prev.from;
    const currSender = curr.from;
    if (prevSender !== currSender) return true;

    // More than 5 minutes apart
    const gap = Date.parse(curr.timestamp) - Date.parse(prev.timestamp);
    return gap > 5 * 60 * 1000;
  }

  function shouldShowDateSeparator(index: number): boolean {
    if (index === 0) return true;
    const prev = messages.value[index - 1];
    const curr = messages.value[index];
    if (!prev || !curr) return true;
    return new Date(prev.timestamp).toDateString() !== new Date(curr.timestamp).toDateString();
  }

  function mergeAndSort(incoming: ChatMessage[], source: string): void {
    const existingKeys = new Set(messages.value.map(m => m._feedKey));
    const newMessages: FeedMessage[] = [];

    for (const msg of incoming) {
      const key = `${msg.id}-${source}`;
      if (!existingKeys.has(key)) {
        newMessages.push({ ...msg, _feedKey: key });
      }
    }

    if (newMessages.length > 0) {
      const combined = [...messages.value, ...newMessages];
      combined.sort((a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp));
      messages.value = combined;
    }
  }

  async function loadAllHistory(): Promise<void> {
    loading.value = true;
    error.value = null;

    try {
      const sources: string[] = [];

      // Gather all conversation JIDs
      for (const convo of conversations.value) {
        sources.push(convo.jid);
      }

      // Also include joined rooms
      for (const roomJid of joinedRooms.value) {
        if (!sources.includes(roomJid)) {
          sources.push(roomJid);
        }
      }

      // Fetch history from all sources in parallel
      const results = await Promise.allSettled(
        sources.map(async (jid) => {
          const history = await getHistory(jid, 50);
          return { jid, history };
        }),
      );

      // Reset and merge
      messages.value = [];
      for (const result of results) {
        if (result.status === 'fulfilled') {
          mergeAndSort(result.value.history, result.value.jid);
        }
      }
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause);
    } finally {
      loading.value = false;
    }
  }

  async function startListening(): Promise<void> {
    // Load initial history
    await loadAllHistory();

    // Listen for new messages
    try {
      const unlisten = await listen<any>('xmpp.message.received', ({ payload }) => {
        const envelope = payload?.payload ?? payload;
        const msg = envelope?.data?.message;
        if (msg) {
          const source = msg.messageType === 'groupchat'
            ? msg.from.split('/')[0]
            : msg.from.split('@')[0] === msg.from.split('/')[0]
              ? msg.to.split('/')[0]
              : msg.from.split('/')[0];
          mergeAndSort([msg], source || msg.from);
        }
      });
      unlistenFns.push(unlisten);
    } catch {
      // Transport not ready
    }

    // Refresh when connection re-establishes
    try {
      const unlisten = await listen('system.connection.established', () => {
        void loadAllHistory();
      });
      unlistenFns.push(unlisten);
    } catch {
      // Transport not ready
    }
  }

  function stopListening(): void {
    while (unlistenFns.length > 0) {
      const fn = unlistenFns.pop();
      fn?.();
    }
  }

  // Reload when conversations or rooms change
  watch(
    [() => conversations.value.length, () => joinedRooms.value.size],
    () => {
      if (conversations.value.length > 0 || joinedRooms.value.size > 0) {
        void loadAllHistory();
      }
    },
  );

  return {
    messages: readonly(messages),
    loading: readonly(loading),
    error: readonly(error),
    shouldShowHeader,
    shouldShowDateSeparator,
    startListening,
    stopListening,
    refresh: loadAllHistory,
  };
}
