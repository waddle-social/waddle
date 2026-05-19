/**
 * Per-user story read state mirrored from the wasm bridge.
 *
 * Stored as a single pubsub item on the user's own JID per XEP-0223;
 * see the design spec at
 * `docs/superpowers/specs/2026-05-19-stories-media-and-reads-design.md`.
 */

export interface StoryRead {
  /** XEP-0501 pubsub item id of the story (non-empty). */
  id: string;
  /** Epoch ms when the story was first marked read. */
  atMs: number;
}

export interface StoryReads {
  entries: readonly StoryRead[];
}

/** Snake-case shape emitted by the wasm bridge (`JsStoryReads`). */
export interface WasmStoryReads {
  entries: WasmStoryRead[];
}

export interface WasmStoryRead {
  id: string;
  /** RFC 3339 timestamp string. */
  at: string;
}

export function storyReadsFromWasm(reads: WasmStoryReads | undefined): StoryReads {
  if (!reads) return { entries: [] };
  const out: StoryRead[] = [];
  for (const entry of reads.entries) {
    if (!entry?.id) continue;
    const atMs = Date.parse(entry.at);
    if (!Number.isFinite(atMs)) continue;
    out.push({ id: entry.id, atMs });
  }
  return { entries: out };
}

export function storyReadsToWasm(reads: StoryReads): WasmStoryReads {
  return {
    entries: reads.entries.map((entry) => ({
      id: entry.id,
      at: new Date(entry.atMs).toISOString(),
    })),
  };
}

/**
 * Drop entries with `atMs < cutoffMs`. Also caps total entries to
 * `max` (oldest first) as a defence-in-depth mirror of the Rust side.
 */
export function pruneStoryReads(
  reads: StoryReads,
  cutoffMs: number,
  max: number,
): StoryReads {
  const kept = reads.entries.filter((entry) => entry.atMs >= cutoffMs);
  if (kept.length <= max) return { entries: kept };
  const sorted = [...kept].sort((a, b) => a.atMs - b.atMs);
  return { entries: sorted.slice(sorted.length - max) };
}

/** 48h prune horizon — stories expire at 24h; doubled for clock skew. */
export const STORY_READS_PRUNE_MS = 48 * 60 * 60 * 1000;

/** Hard cap, mirroring `READ_ENTRY_MAX` in `waddle-xmpp-core`. */
export const STORY_READS_MAX_ENTRIES = 5000;
