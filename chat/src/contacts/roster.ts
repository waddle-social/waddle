import { ref, watch, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { RosterPushEvent } from "@/lib/xmpp/client-events";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { RosterContact } from "@/lib/xmpp/types";
import { $livePresence, liveShow } from "@/presence/live-presence";

const ROSTER_CACHE_PREFIX = "waddle.roster.v1:";

interface PersistedRosterCache {
  ver: string | null;
  contacts: Array<Omit<RosterContact, "presenceShow">>;
}

function cacheKeyForAccount(accountBareJid: string | null | undefined): string | null {
  const bare = accountBareJid ? barePeerJid(accountBareJid) : "";
  return bare ? `${ROSTER_CACHE_PREFIX}${bare}` : null;
}

function readCachedRoster(accountBareJid: string | null | undefined): PersistedRosterCache | null {
  const key = cacheKeyForAccount(accountBareJid);
  if (!key) return null;
  try {
    const raw = window.sessionStorage?.getItem(key);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<PersistedRosterCache>;
    if (!Array.isArray(parsed.contacts)) return null;
    return {
      ver: typeof parsed.ver === "string" ? parsed.ver : null,
      contacts: parsed.contacts.filter(
        (item): item is Omit<RosterContact, "presenceShow"> =>
          Boolean(item && typeof item.jid === "string" && typeof item.username === "string"),
      ),
    };
  } catch {
    return null;
  }
}

function writeCachedRoster(
  accountBareJid: string | null | undefined,
  ver: string | null,
  contacts: readonly RosterContact[],
): void {
  const key = cacheKeyForAccount(accountBareJid);
  if (!key) return;
  try {
    const payload: PersistedRosterCache = {
      ver,
      contacts: contacts.map(({ presenceShow: _presenceShow, ...rest }) => rest),
    };
    window.sessionStorage?.setItem(key, JSON.stringify(payload));
  } catch {
    // best-effort: storage may be unavailable or full
  }
}

function removeCachedRoster(accountBareJid: string | null | undefined): void {
  const key = cacheKeyForAccount(accountBareJid);
  if (!key) return;
  try {
    window.sessionStorage?.removeItem(key);
  } catch {
    // best-effort
  }
}

function seedPresence(
  items: readonly Omit<RosterContact, "presenceShow">[],
  existingByJid?: ReadonlyMap<string, RosterContact["presenceShow"]>,
): RosterContact[] {
  const live = $livePresence.get();
  return items
    .filter((item) => item.subscription !== "remove")
    .map((item) => {
      const bare = barePeerJid(item.jid);
      const presenceShow = liveShow(live, bare) ?? existingByJid?.get(bare);
      return presenceShow !== undefined ? { ...item, jid: bare, presenceShow } : { ...item, jid: bare };
    });
}

export function useXmppRosterContacts(
  xmppClient: Ref<BrowserXmppClient | null>,
  accountBareJid?: Ref<string | null>,
) {
  const contacts = ref<RosterContact[]>([]);
  const isLoadingContacts = ref(false);
  let rosterVersion: string | null = null;
  let loadGeneration = 0;
  let inFlightLoad: { client: BrowserXmppClient; generation: number; promise: Promise<void> } | null = null;

  function hydrateFromCache(account: string | null | undefined) {
    const cached = readCachedRoster(account);
    if (!cached) {
      rosterVersion = null;
      contacts.value = [];
      return;
    }
    rosterVersion = cached.ver;
    contacts.value = seedPresence(cached.contacts);
  }

  if (accountBareJid) {
    watch(
      accountBareJid,
      (account) => {
        loadGeneration += 1;
        inFlightLoad = null;
        isLoadingContacts.value = false;
        hydrateFromCache(account);
      },
      { immediate: true },
    );
  }

  async function loadRosterContacts() {
    const client = xmppClient.value;
    if (!client) {
      contacts.value = [];
      rosterVersion = null;
      return;
    }
    if (inFlightLoad && inFlightLoad.client === client && inFlightLoad.generation === loadGeneration) {
      return inFlightLoad.promise;
    }

    const generation = loadGeneration;
    const account = accountBareJid?.value ?? null;
    isLoadingContacts.value = true;
    const runLoad = async () => {
      try {
        const result = typeof client.listRosterContactsVersioned === "function"
          ? await client.listRosterContactsVersioned(rosterVersion)
          : { unchanged: false, ver: null, contacts: await client.listRosterContacts() };
        if (generation !== loadGeneration || xmppClient.value !== client) return;

        if (result.unchanged) {
          if (result.ver !== null) rosterVersion = result.ver;
          const live = $livePresence.get();
          let changed = false;
          const refreshed = contacts.value.map((contact) => {
            const nextShow = liveShow(live, contact.jid);
            if (nextShow === undefined || contact.presenceShow === nextShow) return contact;
            changed = true;
            return { ...contact, presenceShow: nextShow };
          });
          if (changed) contacts.value = refreshed;
          writeCachedRoster(account, rosterVersion, contacts.value);
          return;
        }

        const existingShowByJid = new Map<string, RosterContact["presenceShow"]>();
        for (const existing of contacts.value) {
          if (existing.presenceShow !== undefined) {
            existingShowByJid.set(existing.jid, existing.presenceShow);
          }
        }
        rosterVersion = result.ver;
        contacts.value = seedPresence(result.contacts, existingShowByJid);
        writeCachedRoster(account, rosterVersion, contacts.value);
      } catch {
        if (generation !== loadGeneration || xmppClient.value !== client) return;
        if (contacts.value.length === 0) {
          contacts.value = [];
        }
      } finally {
        if (inFlightLoad?.client === client && inFlightLoad.generation === generation) {
          inFlightLoad = null;
        }
        if (generation === loadGeneration) {
          isLoadingContacts.value = false;
        }
      }
    };

    const requestPromise = runLoad();
    inFlightLoad = { client, generation, promise: requestPromise };
    return requestPromise;
  }

  function applyRosterPush(push: RosterPushEvent) {
    if (push.ver !== undefined && push.ver !== null) {
      rosterVersion = push.ver;
    }
    if (push.items.length === 0) {
      writeCachedRoster(accountBareJid?.value ?? null, rosterVersion, contacts.value);
      return;
    }
    const live = $livePresence.get();
    const next = [...contacts.value];
    for (const rawItem of push.items) {
      const bare = barePeerJid(rawItem.jid);
      const index = next.findIndex((existing) => existing.jid === bare);
      if (rawItem.subscription === "remove") {
        if (index >= 0) next.splice(index, 1);
        continue;
      }
      const existingShow = index >= 0 ? next[index]?.presenceShow : undefined;
      const presenceShow = liveShow(live, bare) ?? existingShow;
      const merged: RosterContact = presenceShow !== undefined
        ? { ...rawItem, jid: bare, presenceShow }
        : { ...rawItem, jid: bare };
      if (index >= 0) next[index] = merged;
      else next.push(merged);
    }
    contacts.value = next;
    writeCachedRoster(accountBareJid?.value ?? null, rosterVersion, contacts.value);
  }

  function updatePresence(jid: string, show: RosterContact["presenceShow"]) {
    const bareJid = barePeerJid(jid);
    const index = contacts.value.findIndex((contact) => contact.jid === bareJid);
    if (index < 0) return;
    const current = contacts.value[index];
    if (!current || current.presenceShow === show) return;
    const next = [...contacts.value];
    next[index] = { ...current, presenceShow: show };
    contacts.value = next;
  }

  function clearRosterContacts(opts?: { clearPersistedCache?: boolean }) {
    loadGeneration += 1;
    inFlightLoad = null;
    rosterVersion = null;
    contacts.value = [];
    isLoadingContacts.value = false;
    if (opts?.clearPersistedCache) {
      removeCachedRoster(accountBareJid?.value ?? null);
    }
  }

  return {
    contacts,
    isLoadingContacts,
    loadRosterContacts,
    applyRosterPush,
    updatePresence,
    clearRosterContacts,
  };
}
