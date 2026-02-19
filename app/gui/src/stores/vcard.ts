import { ref, computed } from 'vue';
import { defineStore } from 'pinia';
import type { VCardData, VCardSetRequest } from '../composables/useWaddle';

/**
 * Pinia store for vCard data (XEP-0054).
 *
 * Provides cached vCard data keyed by bare JID, with batch fetching,
 * throttling, and display name / avatar resolution helpers.
 */
export const useVCardStore = defineStore('vcard', () => {
  /** vCard cache keyed by bare JID */
  const cache = ref<Map<string, VCardData>>(new Map());

  /** JIDs currently being fetched (for dedup) */
  const fetching = ref<Set<string>>(new Set());

  /** Whether the store has been initialized (own vCard fetched) */
  const initialized = ref(false);

  /** The authenticated user's own bare JID */
  const ownJid = ref<string | null>(null);

  /* ---- transport reference (injected at init) ---- */
  let _getVCard: ((jid: string) => Promise<VCardData | null>) | null = null;
  let _setVCard: ((vcard: VCardSetRequest) => Promise<void>) | null = null;

  /** Initialize the store with transport methods. Call once after login. */
  function init(
    getVCard: (jid: string) => Promise<VCardData | null>,
    setVCard: (vcard: VCardSetRequest) => Promise<void>,
    selfJid: string,
  ) {
    _getVCard = getVCard;
    _setVCard = setVCard;
    ownJid.value = bareJid(selfJid);
    initialized.value = true;
  }

  /** Reset all state (call on disconnect). */
  function reset() {
    cache.value = new Map();
    fetching.value = new Set();
    initialized.value = false;
    ownJid.value = null;
    _getVCard = null;
    _setVCard = null;
  }

  /** Get cached vCard for a JID (reactive). */
  function getVCard(jid: string): VCardData | undefined {
    return cache.value.get(bareJid(jid));
  }

  /** Fetch a single vCard and cache it. Returns cached if available. */
  async function fetchVCard(jid: string): Promise<VCardData | null> {
    const bare = bareJid(jid);
    if (cache.value.has(bare)) return cache.value.get(bare)!;
    if (fetching.value.has(bare)) return null; // already in flight
    if (!_getVCard) return null;

    fetching.value.add(bare);
    try {
      // NFR-2: 5s timeout. Note: the underlying fetch continues in background
      // if timeout fires first, but the result is safely discarded.
      const data = await Promise.race([
        _getVCard(bare),
        timeout(5000),
      ]);
      if (data) {
        cache.value.set(bare, data);
      }
      return data ?? null;
    } catch (err) {
      console.warn(`[vcard-store] Failed to fetch vCard for ${bare}:`, err);
      return null;
    } finally {
      fetching.value.delete(bare);
    }
  }

  /**
   * Batch fetch vCards for multiple JIDs.
   * Throttled to max 5 concurrent (NFR-1).
   * Does not re-fetch already cached JIDs.
   */
  async function fetchBatch(jids: string[]): Promise<void> {
    const MAX_CONCURRENT = 5;
    const toFetch = jids
      .map(bareJid)
      .filter((j) => !cache.value.has(j) && !fetching.value.has(j));

    // Process in chunks of MAX_CONCURRENT
    for (let i = 0; i < toFetch.length; i += MAX_CONCURRENT) {
      const chunk = toFetch.slice(i, i + MAX_CONCURRENT);
      await Promise.allSettled(chunk.map((jid) => fetchVCard(jid)));
    }
  }

  /** Fetch own vCard (FR-2.1). */
  async function fetchOwnVCard(): Promise<VCardData | null> {
    if (!ownJid.value) return null;
    return fetchVCard(ownJid.value);
  }

  /** Update own vCard (FR-3.3). Refreshes cache on success. */
  async function updateOwnVCard(vcard: VCardSetRequest): Promise<void> {
    if (!_setVCard || !ownJid.value) {
      throw new Error('VCard store not initialized');
    }
    await _setVCard(vcard);

    // Invalidate cache and re-fetch to get server-side view
    cache.value.delete(ownJid.value);
    await fetchVCard(ownJid.value);
  }

  /** Invalidate cached vCard (e.g., on XEP-0153 hash change). */
  function invalidate(jid: string) {
    cache.value.delete(bareJid(jid));
  }

  /**
   * Resolve display name for a JID (FR-2.3).
   * Priority: vCard FN → roster name → JID localpart.
   */
  function getDisplayName(jid: string, rosterName?: string | null): string {
    const bare = bareJid(jid);
    const vcard = cache.value.get(bare);

    // 1. vCard FN
    if (vcard?.fullName) return vcard.fullName;

    // 2. Roster name
    if (rosterName) return rosterName;

    // 3. JID localpart
    return bare.split('@')[0] || bare;
  }

  /**
   * Resolve avatar URL for a JID (FR-2.4).
   * Priority: vCard PHOTO → null (caller provides fallback).
   */
  function getAvatarUrl(jid: string): string | null {
    const bare = bareJid(jid);
    const vcard = cache.value.get(bare);
    return vcard?.photoUrl ?? null;
  }

  /** Own display name (reactive computed). */
  const ownDisplayName = computed(() => {
    if (!ownJid.value) return '';
    return getDisplayName(ownJid.value);
  });

  /** Own avatar URL (reactive computed). */
  const ownAvatarUrl = computed(() => {
    if (!ownJid.value) return null;
    return getAvatarUrl(ownJid.value);
  });

  return {
    // State
    cache,
    initialized,
    ownJid,

    // Actions
    init,
    reset,
    getVCard,
    fetchVCard,
    fetchBatch,
    fetchOwnVCard,
    updateOwnVCard,
    invalidate,

    // Getters
    getDisplayName,
    getAvatarUrl,
    ownDisplayName,
    ownAvatarUrl,
  };
});

/* ---- Helpers ---- */

function bareJid(jid: string): string {
  return jid.split('/')[0] || jid;
}

function timeout(ms: number): Promise<null> {
  return new Promise((_, reject) =>
    setTimeout(() => reject(new Error(`vCard fetch timed out after ${ms}ms`)), ms),
  );
}
