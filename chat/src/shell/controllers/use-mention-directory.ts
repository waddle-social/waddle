import { computed, type InjectionKey, type Ref, ref, watch } from "vue";
import type { UserSearchResult } from "@/lib/chat-types";
import type { MentionDirectoryUser } from "@/lib/mentions";

const DEFAULT_DEBOUNCE_MS = 200;

interface MentionDirectoryDeps {
  /** XEP-0055 user-directory search (`XmppClient.searchUsers`). */
  searchUsers: (query: string) => Promise<UserSearchResult[]>;
  /** Identity of the active room, or null when the active conversation is
   * not a room (DMs, group DMs, non-chat pages). Searches only run in rooms. */
  activeRoomId: Ref<string | null>;
  debounceMs?: number;
}

/** What a composer needs: report its active `@query` ("" when closed). */
export interface MentionDirectorySearch {
  search: (query: string) => void;
}

export const mentionDirectoryKey: InjectionKey<MentionDirectorySearch> = Symbol("mention-directory");

/**
 * Offline-mention support for room composers: debounced XEP-0055 directory
 * lookups for the active `@query`, cached per query for the current room.
 * Only the latest query's response is applied; errors fall back to the
 * existing candidates. Every user seen in this room's searches is kept so
 * typed `@label` tokens resolve at send time.
 */
export function useMentionDirectory(deps: MentionDirectoryDeps) {
  const debounceMs = deps.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  const cache = ref<Record<string, MentionDirectoryUser[]>>({});
  const activeQuery = ref("");
  let timer: ReturnType<typeof setTimeout> | null = null;
  let generation = 0;

  function cancelPending() {
    if (timer) clearTimeout(timer);
    timer = null;
    generation += 1;
  }

  watch(deps.activeRoomId, () => {
    cancelPending();
    cache.value = {};
    activeQuery.value = "";
  });

  function search(rawQuery: string) {
    const query = deps.activeRoomId.value ? rawQuery.trim() : "";
    if (query === activeQuery.value) return;
    cancelPending();
    activeQuery.value = query;
    if (!query || Object.hasOwn(cache.value, query)) return;
    const requestGeneration = generation;
    const roomAtRequest = deps.activeRoomId.value;
    timer = setTimeout(async () => {
      timer = null;
      try {
        const users = await deps.searchUsers(query);
        if (requestGeneration !== generation || deps.activeRoomId.value !== roomAtRequest) return;
        cache.value = {
          ...cache.value,
          [query]: users.map((user) => ({ username: user.username, jid: user.jid })),
        };
      } catch {
        // Silent fallback: the room's own candidates stay available.
      }
    }, debounceMs);
  }

  /** Directory users for the active query (empty until its response lands). */
  const results = computed<MentionDirectoryUser[]>(() =>
    activeQuery.value ? cache.value[activeQuery.value] ?? [] : [],
  );
  /** Every directory user seen in this room's searches, first sighting wins. */
  const seenUsers = computed<MentionDirectoryUser[]>(() => {
    const byJid = new Map<string, MentionDirectoryUser>();
    for (const users of Object.values(cache.value)) {
      for (const user of users) {
        const key = user.jid.toLowerCase();
        if (!byJid.has(key)) byJid.set(key, user);
      }
    }
    return [...byJid.values()];
  });

  return { search, results, seenUsers };
}
