import { ref, type Ref } from "vue";
import type { BrowserXmppClient, MessageSearchResult } from "@/lib/xmpp-client";

// XEP-0313 archive query with the `query=` parameter for the channel side.
// The wasm client's `searchMessages(spaceId, channelId, query)` returns
// the matched messages; this composable owns the reactive query/result/
// loading state and the request-token race-id guard that drops stale
// server responses if the user types fast or switches channels mid-query.

type UseChannelMessageSearchDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activeSpaceId: Ref<string | null>;
  activeChannelId: Ref<string | null>;
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
};

export function useChannelMessageSearch(deps: UseChannelMessageSearchDeps) {
  const {
    xmppClient,
    activeSpaceId,
    activeChannelId,
    actionError,
    clearActionError,
    normalizeError,
  } = deps;

  const searchQuery = ref("");
  const searchResults = ref<MessageSearchResult[]>([]);
  const isSearching = ref(false);

  let searchRequestId = 0;

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    if (!client || !channelId) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    searchQuery.value = trimmed;
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    clearActionError();
    try {
      const results = await client.searchMessages(spaceId, channelId, trimmed);
      // Race-id guard: if the user typed again (or switched channels) while
      // this request was in flight, drop the stale result so it can't
      // overwrite the newer query's state.
      if (
        requestId === searchRequestId &&
        xmppClient.value === client &&
        (activeSpaceId.value ?? "") === spaceId &&
        activeChannelId.value === channelId
      ) {
        searchResults.value = results;
      }
    } catch (e) {
      if (requestId === searchRequestId) {
        searchResults.value = [];
        actionError.value = normalizeError(e);
      }
    } finally {
      if (requestId === searchRequestId) {
        isSearching.value = false;
      }
    }
  }

  function clearSearch() {
    searchRequestId++;
    searchQuery.value = "";
    searchResults.value = [];
    isSearching.value = false;
  }

  /**
   * Internal-state reset called by the orchestrator on channel-load /
   * disconnect / clearMessages. Bumps the request token (so any in-flight
   * search response gets dropped on arrival) and clears the public refs.
   */
  function reset() {
    searchRequestId++;
    searchQuery.value = "";
    searchResults.value = [];
    isSearching.value = false;
  }

  return {
    searchQuery,
    searchResults,
    isSearching,
    searchMessages,
    clearSearch,
    reset,
  };
}
