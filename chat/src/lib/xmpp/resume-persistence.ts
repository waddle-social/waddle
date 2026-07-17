/**
 * Persisted resume state across page reloads.
 *
 * Two kinds of state survive a full reload, both keyed per account:
 *
 *   * MAM catch-up cursors (`PersistedReconnectCatchup`) — the
 *     timestamp + optional XEP-0313 archive UID + dedupe seen-ids
 *     for every DM peer and MUC room the user has seen. Without
 *     this, a hard reload (cold start, mobile Safari eviction)
 *     loses every cursor and the next `session:started` returns
 *     `[]` from `ReconnectCatchup.onSessionStarted()` — so MAM
 *     catch-up does NOT run and missed messages are only
 *     re-fetched if the user manually scrolls back.
 *
 *   * XEP-0198 Stream Management resume state
 *     (`PersistedSmResumeState`) — the `previd` plus inbound /
 *     outbound stanza counts and advertised resume window. The richer
 *     "handle" form from the WASM client is a live JS object that cannot
 *     be serialized; the POD `{previd, inboundH, outboundH}` triple can be.
 *     When the native XEP-0198 queue still has unhandled outbound stanzas,
 *     their typed XML token trees and numeric original send instants are
 *     serialized too so `doConnect` can restore sender responsibility after
 *     a full reload without reparsing raw XML.
 *
 * The shape mirrors `outbound-queue-store.ts` (same `waddle.chat.*`
 * prefix family, same per-account key namespacing, same defensive
 * read/write error handling around localStorage availability and
 * quota) so the storage surface stays uniform.
 */

import { reportError } from "@/lib/telemetry";
import type {
  DurableOutcome,
  OutboundOwnerContext,
  OutboundOwnerHandoff,
  OutboundOwnerHint,
} from "@/lib/outbound-durable-store";
import {
  IndexedDbDurableSmResumeStore,
  MemoryDurableSmResumeStore,
  type DurableSmEnvelope,
  type DurableSmResumeStore,
} from "./sm-resume-durable-store";

const CATCHUP_PREFIX = "waddle.chat.resume-cursors";
const SM_PREFIX = "waddle.chat.sm-resume";
const JOINED_ROOMS_PREFIX = "waddle.chat.joined-rooms";
const OWNER_KEY = `${SM_PREFIX}.owner`;
const OWNER_HANDOFF_TOKEN_KEY = `${SM_PREFIX}.owner-handoff-token`;

type PersistedSeenCursor = {
  timestamp: string;
  scope?: "account" | "muc-occupant";
  archiveId?: string;
  archiveTimestamp?: string;
  archiveSeenIds?: string[];
  seenIds?: string[];
};

export type PersistedReconnectCatchup = {
  dmLastSeen: Array<[string, PersistedSeenCursor]>;
  roomLastSeen: Array<[string, PersistedSeenCursor]>;
};

export type XmppResumeStanzaKind = "message" | "presence" | "iq";

type XmppResumeXmlName = {
  namespace: string;
  localName: string;
};

type XmppResumeXmlAttribute = {
  name: XmppResumeXmlName;
  value: string;
};

type XmppResumeXmlToken =
  | { kind: "start"; name: XmppResumeXmlName; attributes: XmppResumeXmlAttribute[] }
  | { kind: "text"; value: string }
  | { kind: "end" };

export type XmppResumeStanza = {
  stanzaKind: XmppResumeStanzaKind;
  tokens: XmppResumeXmlToken[];
};

export type XmppResumeEntry = {
  stanza: XmppResumeStanza;
  sentAtEpochMs: number;
};

/**
 * The on-wire shape that `doConnect` feeds to `with_resume_state`.
 * Stamped with a private `savedAt` internally so a stale POD can be
 * rejected without forcing every caller to know about TTL — see
 * `loadSm`.
 */
export type PersistedSmResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
  maxResumeSeconds?: number;
  unhandledOutboundEntries?: XmppResumeEntry[];
  resource?: string;
};

type ResumeOwner = {
  ownerId: string;
  instanceId: string;
  explicit: boolean;
  handoffToken?: string;
};

/**
 * Fallback resumption window for older persisted PODs that predate
 * `maxResumeSeconds`. This mirrors Waddle server's default XEP-0198
 * max, so old snapshots fail closed instead of lingering for hours.
 */
const DEFAULT_SM_MAX_RESUME_SECONDS = 300;
const SM_SAVED_AT_FUTURE_SKEW_MS = 60_000;
const OWNER_HANDOFF_TTL_MS = 45_000;
const liveOwnerInstances = new Map<string, string>();
let testSmStorageIdentity: Storage | null | undefined;
let testSmStore: MemoryDurableSmResumeStore | null = null;
let testSmStoreToken: string | null = null;
const TEST_SM_STORE_MARKER = `${SM_PREFIX}.bun-store`;

function defaultDurableSmStore(): DurableSmResumeStore {
  // Bun has no browser IndexedDB. Keep the deterministic adapter scoped to
  // the current test's localStorage object so multiple simulated tabs share
  // one transactional store without leaking state between tests.
  if (Reflect.has(globalThis, "Bun")) {
    const identity = storage();
    const marker = identity?.getItem(TEST_SM_STORE_MARKER) ?? null;
    if (!testSmStore || identity !== testSmStorageIdentity || marker !== testSmStoreToken) {
      testSmStorageIdentity = identity;
      testSmStore = new MemoryDurableSmResumeStore();
      testSmStoreToken = randomClaimId();
      identity?.setItem(TEST_SM_STORE_MARKER, testSmStoreToken);
    }
    return testSmStore;
  }
  return new IndexedDbDurableSmResumeStore();
}

function cloneSmState(state: PersistedSmResumeState): PersistedSmResumeState {
  return {
    previd: state.previd,
    inboundH: state.inboundH,
    outboundH: state.outboundH,
    ...(state.maxResumeSeconds === undefined ? {} : { maxResumeSeconds: state.maxResumeSeconds }),
    ...(state.unhandledOutboundEntries?.length
      ? {
          unhandledOutboundEntries: state.unhandledOutboundEntries.map((entry) => ({
            stanza: cloneResumeStanza(entry.stanza),
            sentAtEpochMs: entry.sentAtEpochMs,
          })),
        }
      : {}),
    ...(state.resource ? { resource: state.resource } : {}),
  };
}

function cloneResumeStanza(stanza: XmppResumeStanza): XmppResumeStanza {
  return {
    stanzaKind: stanza.stanzaKind,
    tokens: stanza.tokens.map((token) => {
      if (token.kind === "end") return { kind: "end" };
      if (token.kind === "text") return { kind: "text", value: token.value };
      return {
        kind: "start",
        name: { ...token.name },
        attributes: token.attributes.map((attribute) => ({
          name: { ...attribute.name },
          value: attribute.value,
        })),
      };
    }),
  };
}

export interface ResumePersistence {
  loadCatchup(): PersistedReconnectCatchup | null;
  saveCatchup(snapshot: PersistedReconnectCatchup): void;
  clearCatchup(): void;
  loadSm(): Promise<DurableOutcome<PersistedSmResumeState | null>>;
  consumeSm(): Promise<DurableOutcome<PersistedSmResumeState | null>>;
  saveSm(state: PersistedSmResumeState): Promise<DurableOutcome<void>>;
  clearSm(): Promise<DurableOutcome<boolean>>;
  outboundOwnerHint(): OutboundOwnerHint;
  acceptOutboundOwner(owner: OutboundOwnerContext): void;
  preparePagehideHandoff(): OutboundOwnerHandoff;
  reclaimPagehideOwnership(): void;
  loadJoinedRooms(): string[];
  saveJoinedRooms(roomJids: readonly string[]): void;
  clearJoinedRooms(): void;
}

/** No-op persistence — used in tests / non-browser contexts. */
export const nullResumePersistence: ResumePersistence = {
  loadCatchup: () => null,
  saveCatchup: () => undefined,
  clearCatchup: () => undefined,
  loadSm: async () => ({ kind: "committed", value: null }),
  consumeSm: async () => ({ kind: "committed", value: null }),
  saveSm: async () => ({ kind: "committed", value: undefined }),
  clearSm: async () => ({ kind: "committed", value: false }),
  outboundOwnerHint: () => ({ ownerId: "null-owner", ownerInstanceId: "null-instance" }),
  acceptOutboundOwner: () => undefined,
  preparePagehideHandoff: () => ({ token: "null-handoff", expiresAt: 0 }),
  reclaimPagehideOwnership: () => undefined,
  loadJoinedRooms: () => [],
  saveJoinedRooms: () => undefined,
  clearJoinedRooms: () => undefined,
};

export function createLocalStorageResumePersistence(
  accountKey: string,
  ownerId?: string,
  durableSmStore: DurableSmResumeStore = defaultDurableSmStore(),
): ResumePersistence {
  const owner = ownerId ? explicitResumeOwner(ownerId) : resumeOwner();
  // Length-prefix the account segment so prefix enumeration cannot make
  // `alice@example.com` consume `alice@example.com.evil` shards.
  const catchupKeyPrefix = `${CATCHUP_PREFIX}.${accountKey.length}:${accountKey}`;
  const catchupKey = () => `${catchupKeyPrefix}.${owner.ownerId}`;
  const joinedRoomsKey = () => `${JOINED_ROOMS_PREFIX}.${accountKey}.${owner.ownerId}`;

  return {
    loadCatchup() {
      return readCatchupShards(catchupKeyPrefix);
    },
    saveCatchup(snapshot) {
      writeJson(catchupKey(), snapshot, "catchup");
    },
    clearCatchup() {
      removeKeysWithPrefix(`${catchupKeyPrefix}.`, "catchup");
    },
    async loadSm() {
      const outcome = await durableSmStore.load(accountKey);
      if (outcome.kind === "failed") return outcome;
      const envelope = outcome.value;
      if (
        !envelope
        || envelope.ownerId !== owner.ownerId
        || envelope.consumed
        || !isPersistedSmState(envelope.state)
        || smEnvelopeExpired(envelope)
      ) {
        return { kind: "committed", value: null };
      }
      return { kind: "committed", value: cloneSmState(envelope.state) };
    },
    async consumeSm() {
      const outcome = await durableSmStore.consume(
        accountKey,
        owner.ownerId,
        (envelope) => isPersistedSmState(envelope.state) && !smEnvelopeExpired(envelope),
      );
      if (outcome.kind === "failed") return outcome;
      return {
        kind: "committed",
        value: outcome.value ? cloneSmState(outcome.value.state) : null,
      };
    },
    async saveSm(state) {
      const outcome = await durableSmStore.save(
        accountKey,
        owner.ownerId,
        cloneSmState(state),
        Date.now(),
      );
      return outcome.kind === "failed"
        ? outcome
        : { kind: "committed", value: undefined };
    },
    clearSm() {
      return durableSmStore.clear(accountKey, owner.ownerId);
    },
    outboundOwnerHint() {
      return {
        ownerId: owner.ownerId,
        ownerInstanceId: owner.instanceId,
        ...(owner.handoffToken ? { handoffToken: owner.handoffToken } : {}),
      };
    },
    acceptOutboundOwner(resolved) {
      owner.ownerId = resolved.ownerId;
      owner.instanceId = resolved.ownerInstanceId;
      delete owner.handoffToken;
      writeOwnerSession(owner);
    },
    preparePagehideHandoff() {
      const handoff = {
        token: randomClaimId(),
        expiresAt: Date.now() + OWNER_HANDOFF_TTL_MS,
      };
      owner.handoffToken = handoff.token;
      writeOwnerSession(owner);
      return handoff;
    },
    reclaimPagehideOwnership() {
      delete owner.handoffToken;
      writeOwnerSession(owner);
    },
    loadJoinedRooms() {
      const stored = readJson<string[]>(joinedRoomsKey(), isStringArray, "joined-rooms") ?? [];
      return [...new Set(stored.map(normalizeRoomJid).filter(Boolean))];
    },
    saveJoinedRooms(roomJids) {
      writeJson(
        joinedRoomsKey(),
        [...new Set(roomJids.map(normalizeRoomJid).filter(Boolean))],
        "joined-rooms",
      );
    },
    clearJoinedRooms() {
      removeKey(joinedRoomsKey(), "joined-rooms");
    },
  };
}

function randomClaimId(): string {
  return globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
}

function smEnvelopeExpired(envelope: DurableSmEnvelope): boolean {
  const maxResumeSeconds = envelope.state.maxResumeSeconds ?? DEFAULT_SM_MAX_RESUME_SECONDS;
  const ageMs = Date.now() - envelope.savedAt;
  if (ageMs < -SM_SAVED_AT_FUTURE_SKEW_MS) return true;
  return ageMs > maxResumeSeconds * 1000;
}

function explicitResumeOwner(ownerId: string): ResumeOwner {
  return {
    ownerId,
    instanceId: `explicit:${ownerId}`,
    explicit: true,
  };
}

function resumeOwner(): ResumeOwner {
  const s = sessionStorageForOwner();
  if (!s) {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: ownerInstanceId(ownerId), explicit: false };
  }
  try {
    const ownerId = s.getItem(OWNER_KEY) || randomClaimId();
    const owner: ResumeOwner = {
      ownerId,
      instanceId: ownerInstanceId(ownerId),
      explicit: false,
      ...(s.getItem(OWNER_HANDOFF_TOKEN_KEY)
        ? { handoffToken: s.getItem(OWNER_HANDOFF_TOKEN_KEY)! }
        : {}),
    };
    writeOwnerSession(owner);
    return owner;
  } catch {
    const ownerId = randomClaimId();
    return { ownerId, instanceId: ownerInstanceId(ownerId), explicit: false };
  }
}

function ownerInstanceId(ownerId: string): string {
  const current = liveOwnerInstances.get(ownerId);
  if (current) return current;
  const next = randomClaimId();
  liveOwnerInstances.set(ownerId, next);
  return next;
}

function writeOwnerSession(owner: ResumeOwner): void {
  if (owner.explicit) return;
  const s = sessionStorageForOwner();
  if (!s) return;
  try {
    s.setItem(OWNER_KEY, owner.ownerId);
    if (owner.handoffToken) {
      s.setItem(OWNER_HANDOFF_TOKEN_KEY, owner.handoffToken);
    } else {
      s.removeItem(OWNER_HANDOFF_TOKEN_KEY);
    }
  } catch {
    // IndexedDB owner activation fails closed; sessionStorage is only a hint.
  }
}

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function sessionStorageForOwner(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.sessionStorage ?? null;
  } catch {
    return null;
  }
}

function readJson<T>(key: string, validate: (value: unknown) => value is T, kind: string): T | null {
  const s = storage();
  if (!s) return null;
  try {
    const raw = s.getItem(key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return validate(parsed) ? parsed : null;
  } catch (err) {
    reportError("storage.read", err, {
      recoverable: true,
      detail: `resume-persistence read failed (${kind})`,
      storage_area: kind,
    });
    return null;
  }
}

function storageKeysWithPrefix(prefix: string): string[] {
  const s = storage();
  if (!s) return [];
  const keys: string[] = [];
  try {
    for (let index = 0; index < s.length; index += 1) {
      const key = s.key(index);
      if (key?.startsWith(prefix)) keys.push(key);
    }
  } catch (err) {
    reportError("storage.read", err, {
      recoverable: true,
      detail: "resume-persistence shard enumeration failed (catchup)",
      storage_area: "catchup",
    });
  }
  return keys;
}

function readCatchupShards(prefix: string): PersistedReconnectCatchup | null {
  const snapshots = storageKeysWithPrefix(`${prefix}.`)
    .map((key) => readJson<PersistedReconnectCatchup>(key, isPersistedReconnectCatchup, "catchup"))
    .filter((snapshot): snapshot is PersistedReconnectCatchup => !!snapshot);
  if (snapshots.length === 0) return null;
  return {
    dmLastSeen: snapshots.flatMap((snapshot) => snapshot.dmLastSeen),
    roomLastSeen: snapshots.flatMap((snapshot) => snapshot.roomLastSeen),
  };
}

function removeKeysWithPrefix(prefix: string, kind: string): void {
  for (const key of storageKeysWithPrefix(prefix)) removeKey(key, kind);
}

function writeJson(key: string, value: unknown, kind: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.setItem(key, JSON.stringify(value));
  } catch (err) {
    const name = err instanceof Error ? err.name : "";
    const errorKind = name === "QuotaExceededError" ? "storage.quota" : "storage.write";
    reportError(errorKind, err, {
      recoverable: true,
      detail: `resume-persistence write failed (${kind})`,
      storage_area: kind,
    });
  }
}

function removeKey(key: string, kind: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.removeItem(key);
  } catch (err) {
    reportError("storage.write", err, {
      recoverable: true,
      detail: `resume-persistence clear failed (${kind})`,
      storage_area: kind,
    });
  }
}

function isPersistedSeenCursor(value: unknown): value is PersistedSeenCursor {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (typeof candidate.timestamp !== "string") return false;
  if (candidate.scope !== undefined && candidate.scope !== "account" && candidate.scope !== "muc-occupant") return false;
  if (candidate.archiveId !== undefined && typeof candidate.archiveId !== "string") return false;
  if (candidate.archiveTimestamp !== undefined && typeof candidate.archiveTimestamp !== "string") return false;
  if (candidate.archiveSeenIds !== undefined) {
    if (!Array.isArray(candidate.archiveSeenIds)) return false;
    if (!candidate.archiveSeenIds.every((id) => typeof id === "string")) return false;
  }
  if (candidate.seenIds !== undefined) {
    if (!Array.isArray(candidate.seenIds)) return false;
    if (!candidate.seenIds.every((id) => typeof id === "string")) return false;
  }
  return true;
}

function isCursorMap(value: unknown): value is Array<[string, PersistedSeenCursor]> {
  if (!Array.isArray(value)) return false;
  return value.every((entry) =>
    Array.isArray(entry)
    && entry.length === 2
    && typeof entry[0] === "string"
    && isPersistedSeenCursor(entry[1]),
  );
}

function isPersistedReconnectCatchup(value: unknown): value is PersistedReconnectCatchup {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return isCursorMap(candidate.dmLastSeen) && isCursorMap(candidate.roomLastSeen);
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function normalizeRoomJid(roomJid: string): string {
  return roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
}

function isU32(value: unknown): value is number {
  return typeof value === "number"
    && Number.isInteger(value)
    && value >= 0
    && value <= 0xFFFF_FFFF;
}

const RESUME_XML_TOKEN_LIMIT = 16_384;
const RESUME_XML_DEPTH_LIMIT = 64;
const RESUME_XML_ATTRIBUTE_LIMIT = 16_384;
const JS_DATE_LIMIT_MS = 8_640_000_000_000_000;

function hasOnlyKeys(candidate: Record<string, unknown>, allowed: readonly string[]): boolean {
  const allowedKeys = new Set(allowed);
  return Object.keys(candidate).every((key) => allowedKeys.has(key));
}

function isResumeXmlName(value: unknown): value is XmppResumeXmlName {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return hasOnlyKeys(candidate, ["namespace", "localName"])
    && typeof candidate.namespace === "string"
    && typeof candidate.localName === "string"
    && candidate.localName.length > 0
    && !candidate.localName.includes(":")
    && !/\s/u.test(candidate.localName);
}

function isResumeStanza(value: unknown): value is XmppResumeStanza {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (!hasOnlyKeys(candidate, ["stanzaKind", "tokens"])) return false;
  if (
    candidate.stanzaKind !== "message"
    && candidate.stanzaKind !== "presence"
    && candidate.stanzaKind !== "iq"
  ) return false;
  if (
    !Array.isArray(candidate.tokens)
    || candidate.tokens.length === 0
    || candidate.tokens.length > RESUME_XML_TOKEN_LIMIT
  ) return false;

  let depth = 0;
  let rootSeen = false;
  let attributeCount = 0;
  for (const rawToken of candidate.tokens) {
    if (!rawToken || typeof rawToken !== "object") return false;
    const token = rawToken as Record<string, unknown>;
    if (token.kind === "start") {
      if (!hasOnlyKeys(token, ["kind", "name", "attributes"])) return false;
      if (!isResumeXmlName(token.name) || !Array.isArray(token.attributes)) return false;
      if (depth === 0) {
        if (rootSeen) return false;
        rootSeen = true;
        if (
          token.name.namespace !== "jabber:client"
          || token.name.localName !== candidate.stanzaKind
        ) return false;
      }
      depth += 1;
      if (depth > RESUME_XML_DEPTH_LIMIT) return false;
      attributeCount += token.attributes.length;
      if (attributeCount > RESUME_XML_ATTRIBUTE_LIMIT) return false;
      const expandedNames = new Set<string>();
      for (const rawAttribute of token.attributes) {
        if (!rawAttribute || typeof rawAttribute !== "object") return false;
        const attribute = rawAttribute as Record<string, unknown>;
        if (
          !hasOnlyKeys(attribute, ["name", "value"])
          || !isResumeXmlName(attribute.name)
          || typeof attribute.value !== "string"
        ) return false;
        const expandedName = `${attribute.name.namespace}\0${attribute.name.localName}`;
        if (expandedNames.has(expandedName)) return false;
        expandedNames.add(expandedName);
      }
      continue;
    }
    if (token.kind === "text") {
      if (
        !hasOnlyKeys(token, ["kind", "value"])
        || depth === 0
        || typeof token.value !== "string"
      ) return false;
      continue;
    }
    if (token.kind === "end") {
      if (!hasOnlyKeys(token, ["kind"]) || depth === 0) return false;
      depth -= 1;
      continue;
    }
    return false;
  }
  return rootSeen && depth === 0;
}

function isPersistedSmState(value: unknown): value is PersistedSmResumeState {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (
    typeof candidate.previd !== "string"
    || !isU32(candidate.inboundH)
    || !isU32(candidate.outboundH)
    || (
      candidate.maxResumeSeconds !== undefined
      && (!isU32(candidate.maxResumeSeconds) || candidate.maxResumeSeconds === 0)
    )
    || (candidate.resource !== undefined && typeof candidate.resource !== "string")
  ) return false;
  if (candidate.unhandledOutboundEntries === undefined) return true;
  return Array.isArray(candidate.unhandledOutboundEntries)
    && candidate.unhandledOutboundEntries.every((entry) => {
      if (!entry || typeof entry !== "object") return false;
      const resumeEntry = entry as Record<string, unknown>;
      return hasOnlyKeys(resumeEntry, ["stanza", "sentAtEpochMs"])
        && isResumeStanza(resumeEntry.stanza)
        && typeof resumeEntry.sentAtEpochMs === "number"
        && Number.isSafeInteger(resumeEntry.sentAtEpochMs)
        && Math.abs(resumeEntry.sentAtEpochMs) <= JS_DATE_LIMIT_MS;
    });
}
