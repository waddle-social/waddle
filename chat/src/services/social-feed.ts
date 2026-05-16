/**
 * XEP-0472 Social Feed composable.
 *
 * Wraps the wasm bridge's `feed_items` / `feed_publish` behind a
 * reactive Vue surface so components can render the community feed
 * and post new entries without re-fetching state. Posts published
 * locally are appended to the head of the feed immediately on
 * server-confirm so the UI feels instant.
 */
import { computed, ref, type Ref } from "vue";
import type { BrowserXmppClient, FeedEntry, FeedPostInput } from "@/lib/xmpp-client";

export function useSocialFeed(
  xmppClient: Ref<BrowserXmppClient | null>,
  options: {
    /** Bare JID of the spaces service (e.g. `spaces.waddle.social`). */
    spacesJid: Ref<string | null>;
    /** Max items to fetch per refresh. */
    pageSize?: number;
  },
) {
  const entries = ref<FeedEntry[]>([]);
  const isLoading = ref(false);
  const error = ref<string | null>(null);
  const isPosting = ref(false);
  const pageSize = options.pageSize ?? 50;
  let fetchRequestId = 0;

  const sortedEntries = computed(() => {
    return [...entries.value].sort((a, b) => (b.publishedMs ?? 0) - (a.publishedMs ?? 0));
  });

  async function refresh(): Promise<boolean> {
    const client = xmppClient.value;
    const spacesJid = options.spacesJid.value;
    if (!client || !spacesJid) {
      entries.value = [];
      return false;
    }
    const requestId = ++fetchRequestId;
    isLoading.value = true;
    error.value = null;
    try {
      const fetched = await client.fetchFeed(spacesJid, pageSize);
      if (requestId !== fetchRequestId || client !== xmppClient.value) return false;
      entries.value = fetched;
      return true;
    } catch (err) {
      if (requestId === fetchRequestId) {
        error.value = err instanceof Error ? err.message : String(err);
      }
      return false;
    } finally {
      if (requestId === fetchRequestId) {
        isLoading.value = false;
      }
    }
  }

  async function post(input: FeedPostInput): Promise<FeedEntry | null> {
    const client = xmppClient.value;
    const spacesJid = options.spacesJid.value;
    if (!client || !spacesJid) return null;
    if (!input.body.trim()) return null;
    isPosting.value = true;
    error.value = null;
    try {
      const entry = await client.publishFeedPost(spacesJid, input);
      // Append to local state at the head — UI doesn't need to wait
      // for a refresh round-trip. The caller can trigger a refresh
      // manually (refresh button) when they want to pull in other
      // authors' posts that landed since the last fetch.
      entries.value = [entry, ...entries.value.filter((e) => e.id !== entry.id)];
      return entry;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return null;
    } finally {
      isPosting.value = false;
    }
  }

  function clear() {
    fetchRequestId += 1;
    entries.value = [];
    error.value = null;
    isLoading.value = false;
    isPosting.value = false;
  }

  return {
    entries: sortedEntries,
    isLoading,
    isPosting,
    error,
    refresh,
    post,
    clear,
  };
}
