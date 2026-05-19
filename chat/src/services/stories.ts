/**
 * XEP-0501 Stories composable. Mirrors `useSocialFeed` for the
 * ephemeral stories pubsub node. The composable filters expired
 * entries locally — the wasm bridge returns ALL items, and we
 * re-derive `activeStories` every minute so stories fade out as
 * their countdowns hit zero without a server roundtrip.
 *
 * Read state is synced via a private PEP node (XEP-0223) — see
 * `services/story-read-store.ts`. Marking a story read is fire-and-
 * forget; publishing your own story auto-marks it so other devices
 * don't show it as unread.
 */
import { computed, onScopeDispose, ref, type Ref } from "vue";
import {
  isStoryActive,
  type BrowserXmppClient,
  type Story,
  type StoryPostInput,
} from "@/lib/xmpp-client";
import {
  createStoryReadStore,
  type StoryReadStore,
} from "@/services/story-read-store";

const TICK_INTERVAL_MS = 60_000;

export function useStories(
  xmppClient: Ref<BrowserXmppClient | null>,
  options: {
    communityJid: Ref<string | null>;
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

  const readStore: StoryReadStore = createStoryReadStore(xmppClient);
  // `readVersion` bumps whenever a story is marked read so computed
  // values depending on the read set re-evaluate. The store itself
  // owns a plain Map; this ref is the reactive cursor on top of it.
  const readVersion = ref(0);

  onScopeDispose(() => {
    clearInterval(tickHandle);
    readStore.dispose();
  });

  const activeStories = computed(() => {
    const live = stories.value.filter((s) => isStoryActive(s, nowMs.value));
    return [...live].sort((a, b) => (b.postedMs ?? 0) - (a.postedMs ?? 0));
  });

  function isStoryRead(id: string): boolean {
    void readVersion.value;
    return readStore.isRead(id);
  }

  function markStoryRead(id: string): void {
    if (!id) return;
    if (readStore.isRead(id)) return;
    readStore.markRead(id);
    readVersion.value += 1;
  }

  async function refresh(): Promise<boolean> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    if (!client || !jid) {
      stories.value = [];
      return false;
    }
    const requestId = ++fetchRequestId;
    isLoading.value = true;
    error.value = null;
    try {
      const fetched = await client.fetchStories(jid, pageSize);
      if (requestId !== fetchRequestId || client !== xmppClient.value) return false;
      stories.value = fetched;
      if (!readStore.loaded()) {
        // Hydrate after the first stories fetch so the rail doesn't
        // pop unread→read on first paint.
        await readStore.init();
        readVersion.value += 1;
      }
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
    const jid = options.communityJid.value;
    if (!client || !jid) return null;
    if (!(input.body?.trim() || input.mediaUrl?.trim())) return null;
    isPosting.value = true;
    error.value = null;
    try {
      const story = await client.publishStory(jid, input);
      stories.value = [story, ...stories.value.filter((s) => s.id !== story.id)];
      // Auto-mark: a user should never see their own freshly-posted
      // story as unread on another device. XEP-0501 has no
      // server-side read affordance, so we drive this client-side.
      markStoryRead(story.id);
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
    readStore.dispose();
    readVersion.value += 1;
  }

  return {
    activeStories,
    isLoading,
    isPosting,
    error,
    refresh,
    post,
    clear,
    nowMs,
    isStoryRead,
    markStoryRead,
  };
}
