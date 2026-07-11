import { ref, type Ref } from "vue";
import type { BrowserXmppClient, DmConversationScope, MessageSearchResult } from "@/lib/xmpp-client";

// XEP-0313 archive query with the `query=` parameter for the DM side.
// Mirrors useChannelMessageSearch, scoped to activePeerJid and using
// `searchDmMessages` against the user's personal archive. DM doesn't
// surface a separate `searchQuery` ref (the UI keeps the query in the
// peer-search input directly), so this composable only exposes results /
// loading state.

type UseDmMessageSearchDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  conversationScope?: (peerJid: string) => DmConversationScope;
};

export function useDmMessageSearch(deps: UseDmMessageSearchDeps) {
  const {
    xmppClient,
    activePeerJid,
    actionError,
    clearActionError,
    normalizeError,
    conversationScope,
  } = deps;

  const searchResults = ref<MessageSearchResult[]>([]);
  const isSearching = ref(false);

  let searchRequestId = 0;

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    clearActionError();
    try {
      const results = await client.searchDmMessages(
        peerJid,
        trimmed,
        20,
        conversationScope?.(peerJid),
      );
      // Race-id guard: same purpose as the channel side — drop stale
      // results from earlier queries / different peers.
      if (
        requestId === searchRequestId &&
        xmppClient.value === client &&
        activePeerJid.value === peerJid
      ) {
        searchResults.value = results;
      }
    } catch (e) {
      if (requestId === searchRequestId) {
        searchResults.value = [];
        actionError.value = normalizeError(e);
      }
    } finally {
      if (requestId === searchRequestId) isSearching.value = false;
    }
  }

  function clearSearch() {
    searchRequestId++;
    searchResults.value = [];
    isSearching.value = false;
  }

  function reset() {
    searchRequestId++;
    searchResults.value = [];
    isSearching.value = false;
  }

  return {
    searchResults,
    isSearching,
    searchMessages,
    clearSearch,
    reset,
  };
}
