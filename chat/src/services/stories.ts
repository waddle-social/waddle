/**
 * XEP-0501 Stories composable.
 *
 * Mirrors `useSocialFeed` for the ephemeral stories pubsub node.
 * The composable filters expired entries locally — the wasm bridge
 * returns ALL items, and we re-derive `active` every minute so
 * stories fade out as their countdowns hit zero without a server
 * roundtrip.
 */
import { computed, onScopeDispose, ref, type Ref } from "vue";
import { isStoryActive, type BrowserXmppClient, type Story, type StoryPostInput } from "@/lib/xmpp-client";

const TICK_INTERVAL_MS = 60_000;

export function useStories(
  xmppClient: Ref<BrowserXmppClient | null>,
  options: {
    /** Bare JID of the spaces service. */
    spacesJid: Ref<string | null>;
    /** Max items to fetch per refresh. */
    pageSize?: number;
  },
) {
  const stories = ref<Story[]>([]);
  const isLoading = ref(false);
  const isPosting = ref(false);
  const error = ref<string | null>(null);
  const pageSize = options.pageSize ?? 50;
  let fetchRequestId = 0;
  const nowMs = ref(Date.now());
  const tickHandle = setInterval(() => {
    nowMs.value = Date.now();
  }, TICK_INTERVAL_MS);
  onScopeDispose(() => clearInterval(tickHandle));

  const activeStories = computed(() => {
    const live = stories.value.filter((s) => isStoryActive(s, nowMs.value));
    return [...live].sort((a, b) => (b.postedMs ?? 0) - (a.postedMs ?? 0));
  });

  async function refresh(): Promise<boolean> {
    const client = xmppClient.value;
    const spacesJid = options.spacesJid.value;
    if (!client || !spacesJid) {
      stories.value = [];
      return false;
    }
    const requestId = ++fetchRequestId;
    isLoading.value = true;
    error.value = null;
    try {
      const fetched = await client.fetchStories(spacesJid, pageSize);
      if (requestId !== fetchRequestId || client !== xmppClient.value) return false;
      stories.value = fetched;
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

  async function post(input: StoryPostInput): Promise<Story | null> {
    const client = xmppClient.value;
    const spacesJid = options.spacesJid.value;
    if (!client || !spacesJid) return null;
    if (!(input.body?.trim() || input.mediaUrl?.trim())) return null;
    isPosting.value = true;
    error.value = null;
    try {
      const story = await client.publishStory(spacesJid, input);
      stories.value = [story, ...stories.value.filter((s) => s.id !== story.id)];
      return story;
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      return null;
    } finally {
      isPosting.value = false;
    }
  }

  function clear() {
    fetchRequestId += 1;
    stories.value = [];
    error.value = null;
    isLoading.value = false;
    isPosting.value = false;
  }

  return {
    activeStories,
    isLoading,
    isPosting,
    error,
    refresh,
    post,
    clear,
    /** Exposed so tests can advance time without waiting for the interval. */
    nowMs,
  };
}
