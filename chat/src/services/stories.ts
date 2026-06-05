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
  normalizeStoryReactions,
  STORY_REACTIONS_MAX,
  type BrowserXmppClient,
  type Story,
  type StoryPostInput,
  type StoryReactionItem,
  type StoryReactionSummary,
} from "@/lib/xmpp-client";
import {
  createStoryReadStore,
  type StoryReadStore,
} from "@/services/story-read-store";

const TICK_INTERVAL_MS = 60_000;
const REACTION_FETCH_BATCH_SIZE = 8;

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
  const reactionsByStory = ref<Record<string, StoryReactionItem[]>>({});
  const reactionSummariesByStory = ref<Record<string, StoryReactionSummary>>({});
  const pageSize = options.pageSize ?? 50;
  let fetchRequestId = 0;
  const reactionMutationVersionsByStory = new Map<string, number>();
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
      await refreshReactionSummariesFor(fetched, client, jid, requestId);
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

  async function refreshReactionSummariesFor(
    fetched: readonly Story[],
    client: BrowserXmppClient,
    communityJid: string,
    requestId: number,
  ): Promise<void> {
    const mutationVersions = new Map(reactionMutationVersionsByStory);
    const entries: Array<readonly [string, StoryReactionSummary, StoryReactionItem | null]> = [];
    for (let offset = 0; offset < fetched.length; offset += REACTION_FETCH_BATCH_SIZE) {
      if (requestId !== fetchRequestId || client !== xmppClient.value) return;
      const batch = fetched.slice(offset, offset + REACTION_FETCH_BATCH_SIZE);
      entries.push(...await Promise.all(
        batch.map(async (story): Promise<readonly [string, StoryReactionSummary, StoryReactionItem | null]> => {
          let summary: StoryReactionSummary = { counts: {}, reactors: {}, mine: [] };
          let selfItem: StoryReactionItem | null = null;
          const [fetchedSummary, fetchedSelf] = await Promise.all([
            client.fetchStoryReactionSummary(communityJid, story.id).catch(() => null),
            client.bareJid
              ? client.fetchMyStoryReactions(communityJid, story.id).catch(() => null)
              : Promise.resolve(null),
          ]);
          if (fetchedSummary) summary = fetchedSummary;
          selfItem = fetchedSelf;
          return [story.id, summary, selfItem] as const;
        }),
      ));
    }
    if (requestId !== fetchRequestId || client !== xmppClient.value) return;
    const { summaries, selfItems } = mergedReactionSummaryEntries(entries, mutationVersions, client.bareJid);
    reactionSummariesByStory.value = summaries;
    reactionsByStory.value = selfItems;
  }

  function mergedReactionSummaryEntries(
    entries: Array<readonly [string, StoryReactionSummary, StoryReactionItem | null]>,
    mutationVersions: ReadonlyMap<string, number>,
    self: string | null | undefined,
  ): { summaries: Record<string, StoryReactionSummary>; selfItems: Record<string, StoryReactionItem[]> } {
    if (!self) {
      return {
        summaries: Object.fromEntries(entries.map(([storyId, fetchedSummary]) => [storyId, fetchedSummary])),
        selfItems: {},
      };
    }
    const selfKey = self.toLowerCase();
    const pairs = entries.map(([storyId, fetchedSummary, fetchedSelf]) => {
        if (mutationVersions.get(storyId) === reactionMutationVersionsByStory.get(storyId)) {
          const selfItems = fetchedSelf ? [fetchedSelf] : [];
          return [storyId, { ...fetchedSummary, mine: fetchedSelf?.emojis ?? [] }, selfItems] satisfies [string, StoryReactionSummary, StoryReactionItem[]];
        }
        const currentSelf = (reactionsByStory.value[storyId] ?? [])
          .find((item) => item.jid.toLowerCase() === selfKey);
        if (!currentSelf) {
          const counts = applySelfReactionDelta(fetchedSummary.counts, fetchedSelf?.emojis ?? [], []);
          return [storyId, { ...fetchedSummary, counts, mine: [] }, []] satisfies [string, StoryReactionSummary, StoryReactionItem[]];
        }
        const counts = applySelfReactionDelta(fetchedSummary.counts, fetchedSelf?.emojis ?? [], currentSelf.emojis);
        return [storyId, { ...fetchedSummary, counts, mine: currentSelf.emojis }, [currentSelf]] satisfies [string, StoryReactionSummary, StoryReactionItem[]];
      });
    return {
      summaries: Object.fromEntries(pairs.map(([storyId, summary]) => [storyId, summary])),
      selfItems: Object.fromEntries(pairs.map(([storyId, , selfItems]) => [storyId, selfItems])),
    };
  }

  function applySelfReactionDelta(
    fetchedCounts: StoryReactionSummary["counts"],
    fetchedSelfEmojis: readonly string[],
    currentSelfEmojis: readonly string[],
  ): StoryReactionSummary["counts"] {
    const counts = { ...fetchedCounts };
    for (const emoji of normalizeStoryReactions(fetchedSelfEmojis)) {
      const next = (counts[emoji] ?? 0) - 1;
      if (next > 0) counts[emoji] = next;
      else delete counts[emoji];
    }
    for (const emoji of normalizeStoryReactions(currentSelfEmojis)) {
      counts[emoji] = (counts[emoji] ?? 0) + 1;
    }
    return counts;
  }

  function reactionSummary(storyId: string): StoryReactionSummary {
    return reactionSummariesByStory.value[storyId] ?? { counts: {}, reactors: {}, mine: [] };
  }

  async function toggleReaction(storyId: string, emoji: string): Promise<boolean> {
    const client = xmppClient.value;
    const jid = options.communityJid.value;
    const self = client?.bareJid;
    if (!client || !jid || !self || !storyId || !emoji.trim()) return false;
    const previousItems = reactionsByStory.value[storyId] ?? [];
    const existing = previousItems.find((item) => item.jid.toLowerCase() === self.toLowerCase());
    const current = normalizeStoryReactions(existing?.emojis ?? []);
    const next = current.includes(emoji)
      ? current.filter((candidate) => candidate !== emoji)
      : normalizeStoryReactions([...current, emoji]);
    if (next.length > STORY_REACTIONS_MAX) return false;
    const nextItems = next.length > 0
      ? [
          ...previousItems.filter((item) => item.jid.toLowerCase() !== self.toLowerCase()),
          { jid: self, emojis: next, unknownChildrenXml: existing?.unknownChildrenXml ?? [] },
        ]
      : previousItems.filter((item) => item.jid.toLowerCase() !== self.toLowerCase());

    const mutationVersion = (reactionMutationVersionsByStory.get(storyId) ?? 0) + 1;
    reactionMutationVersionsByStory.set(storyId, mutationVersion);
    const previousSummary = reactionSummariesByStory.value[storyId] ?? { counts: {}, reactors: {}, mine: [] };
    reactionSummariesByStory.value = {
      ...reactionSummariesByStory.value,
      [storyId]: optimisticSummary(previousSummary, current, next),
    };
    reactionsByStory.value = { ...reactionsByStory.value, [storyId]: nextItems };
    try {
      if (next.length > 0) {
        await client.publishStoryReactions(jid, storyId, next, existing?.unknownChildrenXml ?? []);
      } else {
        await client.retractStoryReactions(jid, storyId);
      }
      return true;
    } catch (err) {
      if (reactionMutationVersionsByStory.get(storyId) === mutationVersion) {
        reactionsByStory.value = { ...reactionsByStory.value, [storyId]: previousItems };
        reactionSummariesByStory.value = {
          ...reactionSummariesByStory.value,
          [storyId]: previousSummary,
        };
      }
      error.value = err instanceof Error ? err.message : String(err);
      return false;
    }
  }

  function clear() {
    fetchRequestId += 1;
    stories.value = [];
    reactionsByStory.value = {};
    reactionSummariesByStory.value = {};
    reactionMutationVersionsByStory.clear();
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
    reactionSummary,
    toggleReaction,
  };
}

function optimisticSummary(
  previous: StoryReactionSummary,
  currentMine: readonly string[],
  nextMine: readonly string[],
): StoryReactionSummary {
  const counts = { ...previous.counts };
  for (const emoji of normalizeStoryReactions(currentMine)) {
    counts[emoji] = Math.max(0, (counts[emoji] ?? 0) - 1);
    if (counts[emoji] === 0) delete counts[emoji];
  }
  for (const emoji of normalizeStoryReactions(nextMine)) {
    counts[emoji] = (counts[emoji] ?? 0) + 1;
  }
  return { ...previous, counts, mine: nextMine };
}
