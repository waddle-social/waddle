/**
 * Per-user story read-state store backed by a private PEP node
 * (XEP-0223 `urn:waddle:story:reads:0`). The store exposes a small,
 * UI-facing surface (is/mark/loaded) and hides the wasm/IQ details
 * behind it; the same interface is what the spec uses to keep
 * `StoriesPane.vue` free of XMPP concerns.
 *
 * Publish strategy: marks are added to the in-memory set immediately,
 * and a debounced publish coalesces bursts so opening N stories in a
 * row produces one IQ. Failures are swallowed — read state is not
 * load-bearing.
 *
 * Pruning: on every publish the wire payload is pruned to entries
 * within the last 48h and capped at 5000 entries, mirroring the Rust
 * `prune_before` + `cap_to` defence-in-depth.
 */

import {
  pruneStoryReads,
  STORY_READS_MAX_ENTRIES,
  STORY_READS_PRUNE_MS,
  type BrowserXmppClient,
  type StoryRead,
  type StoryReads,
} from "@/lib/xmpp-client";

const DEFAULT_PUBLISH_DEBOUNCE_MS = 250;

export interface StoryReadStore {
  /** Reactive: true once `init()` has resolved (success or empty). */
  readonly loaded: () => boolean;
  /** Iterate the current set of read story ids. */
  readonly readIds: () => ReadonlySet<string>;
  /** Test membership without copying the set. */
  isRead(id: string): boolean;
  /**
   * Mark a story as read locally and schedule a debounced publish.
   * Idempotent on the same id within the debounce window.
   */
  markRead(id: string, atMs?: number): void;
  /** Hydrate from the user's PEP node. Safe to call once per session. */
  init(): Promise<void>;
  /** Flush any pending debounced publish immediately. */
  flush(): Promise<void>;
  /** Cancel pending timers and drop state. Used on logout. */
  dispose(): void;
}

interface StoryReadStoreOptions {
  publishDebounceMs?: number;
  /** Override `Date.now` for tests. */
  now?: () => number;
}

export function createStoryReadStore(
  xmppClient: { value: BrowserXmppClient | null },
  options: StoryReadStoreOptions = {},
): StoryReadStore {
  const debounceMs = options.publishDebounceMs ?? DEFAULT_PUBLISH_DEBOUNCE_MS;
  const now = options.now ?? (() => Date.now());

  const reads = new Map<string, number>();
  let loadedFlag = false;
  let publishTimer: ReturnType<typeof setTimeout> | null = null;
  let publishInFlight: Promise<void> | null = null;
  let dirty = false;

  function snapshot(): StoryReads {
    const entries: StoryRead[] = [];
    for (const [id, atMs] of reads) entries.push({ id, atMs });
    return pruneStoryReads(
      { entries },
      now() - STORY_READS_PRUNE_MS,
      STORY_READS_MAX_ENTRIES,
    );
  }

  async function publishNow(): Promise<void> {
    const client = xmppClient.value;
    if (!client) return;
    const payload = snapshot();
    // Reflect pruning back into the in-memory map so the store and the
    // server agree on what's stored.
    reads.clear();
    for (const entry of payload.entries) reads.set(entry.id, entry.atMs);
    try {
      await client.publishStoryReads(payload);
    } catch {
      // Read state is non-critical — surfacing the error to the UI
      // would just create noise. The next mark will retry.
    }
  }

  function schedulePublish(): void {
    if (publishTimer !== null) clearTimeout(publishTimer);
    publishTimer = setTimeout(() => {
      publishTimer = null;
      if (!dirty) return;
      dirty = false;
      publishInFlight = publishNow().finally(() => {
        publishInFlight = null;
      });
    }, debounceMs);
  }

  return {
    loaded: () => loadedFlag,
    readIds: () => new Set(reads.keys()),
    isRead: (id) => reads.has(id),
    markRead(id: string, atMs?: number) {
      if (!id) return;
      const t = atMs ?? now();
      const existing = reads.get(id);
      if (existing !== undefined && existing >= t) return;
      reads.set(id, t);
      dirty = true;
      schedulePublish();
    },
    async init(): Promise<void> {
      const client = xmppClient.value;
      if (!client) {
        loadedFlag = true;
        return;
      }
      try {
        const fetched = await client.fetchStoryReads();
        // Apply prune horizon at hydrate too so a long-disconnected
        // client doesn't load 48+ hour old entries.
        const pruned = pruneStoryReads(
          fetched,
          now() - STORY_READS_PRUNE_MS,
          STORY_READS_MAX_ENTRIES,
        );
        reads.clear();
        for (const entry of pruned.entries) reads.set(entry.id, entry.atMs);
      } catch {
        // Non-critical: leave the set empty.
      } finally {
        loadedFlag = true;
      }
    },
    async flush(): Promise<void> {
      if (publishTimer !== null) {
        clearTimeout(publishTimer);
        publishTimer = null;
        if (dirty) {
          dirty = false;
          publishInFlight = publishNow().finally(() => {
            publishInFlight = null;
          });
        }
      }
      if (publishInFlight) await publishInFlight;
    },
    dispose() {
      if (publishTimer !== null) {
        clearTimeout(publishTimer);
        publishTimer = null;
      }
      reads.clear();
      loadedFlag = false;
      dirty = false;
    },
  };
}
