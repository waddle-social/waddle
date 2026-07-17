import type { PersistedQueuedMessage } from "./outbound-queue-store";
import type { PersistedSmResumeState } from "./xmpp/sm-resume-types";
import {
  cloneSmResumeState,
  decodePersistedSmResumeState,
} from "./xmpp/sm-resume-types";

const DATABASE_NAME = "waddle-chat-xmpp-runtime-v1";
const DATABASE_VERSION = 1;
const ACCOUNT_STORE_NAME = "accounts";
const DEFAULT_SM_RESUME_WINDOW_MS = 300_000;

export const OUTBOUND_CLAIM_LEASE_MS = 45_000;
const OUTBOUND_OWNER_RETENTION_MS = 8 * 24 * 60 * 60 * 1_000;
export const SM_SNAPSHOT_RETENTION_MS = 8 * 24 * 60 * 60 * 1_000;
const AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS = 1_000;
export const RETAINED_PREDECESSOR_LIMIT = 64;

export function checkedDurableCounterIncrement(
  value: number,
  label: string,
): number {
  if (
    !Number.isSafeInteger(value)
    || value < 0
    || value >= Number.MAX_SAFE_INTEGER
  ) {
    throw new DOMException(`${label} counter exhausted`, "AbortError");
  }
  return value + 1;
}

function checkedDurableDeadline(
  start: number,
  durationMs: number,
  label: string,
): number {
  if (
    !Number.isSafeInteger(start)
    || start < 0
    || !Number.isSafeInteger(durationMs)
    || durationMs < 0
    || start > Number.MAX_SAFE_INTEGER - durationMs
  ) {
    throw new DOMException(`${label} deadline exhausted`, "AbortError");
  }
  return start + durationMs;
}

export type DurableFailureReason =
  | "unavailable"
  | "quota"
  | "security"
  | "capacity"
  | "aborted";

export type DurableOutcome<T> =
  | { kind: "committed"; value: T }
  | { kind: "failed"; reason: DurableFailureReason; cause?: unknown };

export class OutboundPersistenceError extends Error {
  constructor(
    readonly operation: string,
    readonly reason: DurableFailureReason,
    readonly cause?: unknown,
  ) {
    super(`Outbound persistence ${operation} failed: ${reason}`);
    this.name = "OutboundPersistenceError";
  }
}

export class DurablePredecessorCapacityError extends Error {
  constructor(readonly limit = RETAINED_PREDECESSOR_LIMIT) {
    super(`Durable predecessor capacity ${limit} is exhausted`);
    this.name = "DurablePredecessorCapacityError";
  }
}

export function committedOrThrow<T>(operation: string, outcome: DurableOutcome<T>): T {
  if (outcome.kind === "committed") return outcome.value;
  throw new OutboundPersistenceError(operation, outcome.reason, outcome.cause);
}

/** Minimal fence copied into claims, intents, and SM mutations. */
type OutboundOwnerFence = {
  accountKey: string;
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
  authorityEpoch: number;
};

/**
 * Activation-only payload. A pagehide SM snapshot may be large and must never
 * be spread into a per-message claim or terminal intent.
 */
export type OutboundOwnerActivation = {
  fence: OutboundOwnerFence;
  /**
   * Present only on the one successor that atomically consumed a valid
   * pagehide handoff. It is already marked consumed in durable storage.
   */
  handoffSm?: DurableSmEnvelope;
};

export type OutboundOwnerContext = OutboundOwnerFence;

export type OutboundOwnerHint = {
  ownerId: string;
  ownerInstanceId: string;
  handoffToken?: string;
};

type OutboundOwnerHandoff = {
  token: string;
  expiresAt: number;
  authorityEpoch: number;
  ownerGeneration: number;
};

export type OutboundClaimPhase =
  | "sending"
  | "resume-replay"
  | "fresh-fallback";

export type OutboundClaimRequest = OutboundOwnerContext & {
  connectionGeneration: number;
  claimId: string;
  phase: OutboundClaimPhase;
};

export type OutboundRowIdentity = {
  accountKey: string;
  messageId: string;
  incarnation: string;
  payloadDigest: string;
};

export type OutboundClaim = OutboundClaimRequest & {
  rowIncarnation: string;
  payloadDigest: string;
  leaseUntil: number;
};

export type OutboundLane =
  | { kind: "direct" }
  | { kind: "room"; roomJid: string };

type DurableOutboundEntryState =
  | { kind: "ready" }
  | { kind: "claimed"; claim: OutboundClaim }
  | { kind: "terminal"; intentId: string };

type DurableOutboundEntry = {
  identity: OutboundRowIdentity;
  lane: OutboundLane;
  message: PersistedQueuedMessage;
  state: DurableOutboundEntryState;
};

type DurableOutboundScan = {
  entries: DurableOutboundEntry[];
  pruned: OutboundRowIdentity[];
  revision: number;
};

type OutboundPersistResult =
  | { kind: "inserted"; entry: DurableOutboundEntry }
  | { kind: "existing"; entry: DurableOutboundEntry }
  | {
      kind: "conflict";
      messageId: string;
      existingPayloadDigest: string;
      attemptedPayloadDigest: string;
    };

type OutboundPersistClaimedResult =
  | { kind: "claimed"; entry: DurableOutboundEntry; claim: OutboundClaim }
  | { kind: "busy"; entry: DurableOutboundEntry; leaseUntil: number }
  | { kind: "terminal"; entry: DurableOutboundEntry }
  | { kind: "fenced" }
  | Extract<OutboundPersistResult, { kind: "conflict" }>;

type OutboundClaimHeadResult =
  | { kind: "claimed"; entry: DurableOutboundEntry; claim: OutboundClaim }
  | { kind: "busy"; messageId: string; leaseUntil: number }
  | { kind: "missing" }
  | { kind: "terminal"; messageId: string }
  | { kind: "fenced" };

type OutboundRenewResult =
  | { kind: "renewed"; claim: OutboundClaim }
  | { kind: "missing" }
  | { kind: "fenced" };

type OutboundReleaseResult =
  | { kind: "released" }
  | { kind: "missing" }
  | { kind: "fenced" };

type ResumeClaimReconciliation =
  | {
      kind: "reconciled";
      claims: Array<{ messageId: string; claim: OutboundClaim }>;
      releasedIds: string[];
      blockedIds: string[];
      terminalIds: string[];
      missingIds: string[];
    }
  | { kind: "fenced" };

type OutboundTerminalKind =
  | "ack"
  | "native-failure"
  | "nonretryable-delete";

export type OutboundTerminalIntent = {
  intentId: string;
  accountKey: string;
  identity: OutboundRowIdentity;
  kind: OutboundTerminalKind;
  expected: OutboundClaim;
  recordedAt: number;
};

type OutboundTerminalRecordResult =
  | { kind: "recorded"; intent: OutboundTerminalIntent }
  | { kind: "missing" }
  | { kind: "stale" }
  | { kind: "fenced" };

export type OutboundTerminalApplyResult =
  | { kind: "acked"; identity: OutboundRowIdentity }
  | { kind: "removed"; identity: OutboundRowIdentity }
  | { kind: "released"; identity: OutboundRowIdentity }
  | { kind: "fallback"; identity: OutboundRowIdentity; claim: OutboundClaim }
  | { kind: "missing" }
  | { kind: "stale" }
  | { kind: "fenced" };

export type DurableSmEnvelope = {
  accountKey: string;
  ownerId: string;
  ownerGeneration: number;
  authorityEpoch: number;
  version: number;
  state: PersistedSmResumeState;
  savedAt: number;
  consumed: boolean;
};

type DurableSmLoadResult =
  | { kind: "loaded"; envelope: DurableSmEnvelope | null; version: number | null }
  | { kind: "fenced" };

type DurableSmClearResult = {
  cleared: boolean;
  version: number;
};

type DurableSmMutationResult<T> =
  | { kind: "applied"; value: T }
  | { kind: "stale"; actualVersion: number | null }
  | { kind: "fenced" };

export type PagehideHandoffResult = {
  handoff: OutboundOwnerHandoff;
  smVersion: number;
};

type PagehideHandoffCancelResult =
  | { kind: "applied"; cancelled: boolean }
  | {
      kind: "stale";
      actualToken: string | null;
      actualSmVersion: number | null;
    }
  | { kind: "fenced" };

interface DurableSmResumeStore {
  loadSm(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<DurableSmLoadResult>>;
  consumeSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope | null>>>;
  saveSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    state: PersistedSmResumeState,
    _savedAt: number,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope>>>;
  clearSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmClearResult>>>;
}

export interface DurableOutboundStore extends DurableSmResumeStore {
  revision(accountKey: string): Promise<DurableOutcome<number>>;
  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>>;
  scanAndPrune(
    accountKey: string,
    cutoff: number,
  ): Promise<DurableOutcome<DurableOutboundScan>>;
  persistReady(
    accountKey: string,
    message: PersistedQueuedMessage,
  ): Promise<DurableOutcome<OutboundPersistResult>>;
  persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundPersistClaimedResult>>;
  claimHead(
    accountKey: string,
    lane: OutboundLane,
    claim: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimHeadResult>>;
  renew(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundRenewResult>>;
  release(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundReleaseResult>>;
  reconcileResumeClaims(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
    authoritativeMessageIds: readonly string[] | null,
    phase: Extract<OutboundClaimPhase, "resume-replay" | "fresh-fallback">,
  ): Promise<DurableOutcome<ResumeClaimReconciliation>>;
  releaseForFreshSession(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
  ): Promise<DurableOutcome<string[] | null>>;
  listTerminal(
    accountKey: string,
  ): Promise<DurableOutcome<OutboundTerminalIntent[]>>;
  recordTerminal(
    identity: OutboundRowIdentity,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>>;
  applyTerminal(
    executor: OutboundOwnerContext,
    intent: OutboundTerminalIntent,
  ): Promise<DurableOutcome<OutboundTerminalApplyResult>>;
  claimOwner(
    accountKey: string,
    hint: OutboundOwnerHint,
  ): Promise<DurableOutcome<OutboundOwnerActivation>>;
  renewOwner(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<boolean>>;
  preparePagehideHandoff(
    owner: OutboundOwnerContext,
    expectedSmVersion: number | null,
    handoffToken: string,
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<PagehideHandoffResult>>>;
  cancelOwnerHandoff(
    owner: OutboundOwnerContext,
    expectedToken: string,
    expectedSmVersion: number,
  ): Promise<DurableOutcome<PagehideHandoffCancelResult>>;
}

type DurableOutboundRow = {
  identity: OutboundRowIdentity;
  lane: OutboundLane;
  orderKey: string;
  message: PersistedQueuedMessage;
  state: DurableOutboundEntryState;
};

type DurableOutboundOwner = {
  ownerId: string;
  ownerInstanceId: string;
  ownerGeneration: number;
  authorityEpoch: number;
  leaseUntil: number;
  lastRenewedAt: number;
  handoff?: OutboundOwnerHandoff;
  predecessors?: Array<{
    ownerInstanceId: string;
    ownerGeneration: number;
    authorityEpoch: number;
    expiresAt: number;
  }>;
};

type DurablePredecessorFence =
  NonNullable<DurableOutboundOwner["predecessors"]>[number];

type DurableSmRecord = {
  accountKey: string;
  ownerId: string;
  ownerGeneration: number;
  authorityEpoch: number;
  version: number;
  state: PersistedSmResumeState | null;
  savedAt: number;
  consumed: boolean;
};

type RuntimeAccount = {
  accountKey: string;
  schemaVersion: 1;
  revision: number;
  lastAuthorityTimeMs: number;
  lastWallClockSampleMs: number;
  authorityEpoch: number;
  nextOwnerGeneration: number;
  owners: Record<string, DurableOutboundOwner>;
  outbound: Record<string, DurableOutboundRow>;
  terminals: Record<string, OutboundTerminalIntent>;
  smSnapshots: Record<string, DurableSmRecord>;
};

type AccountMutation<T> = {
  changed: boolean;
  value: T;
  finalize?: (committedRevision: number) => T;
};

function dictionary<T>(): Record<string, T> {
  return Object.create(null) as Record<string, T>;
}

function emptyAccount(accountKey: string): RuntimeAccount {
  return {
    accountKey,
    schemaVersion: 1,
    revision: 0,
    lastAuthorityTimeMs: 0,
    lastWallClockSampleMs: 0,
    authorityEpoch: 0,
    nextOwnerGeneration: 1,
    owners: dictionary(),
    outbound: dictionary(),
    terminals: dictionary(),
    smSnapshots: dictionary(),
  };
}

function corruptRuntimeAccount(detail: string): never {
  throw new DOMException(
    `Corrupt XMPP runtime account: ${detail}`,
    "DataError",
  );
}

function strictObject(
  value: unknown,
  allowedKeys: readonly string[],
  detail: string,
): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return corruptRuntimeAccount(`${detail} is not an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    return corruptRuntimeAccount(`${detail} has a custom prototype`);
  }
  const record = value as Record<string, unknown>;
  const allowed = new Set(allowedKeys);
  for (const key of Object.keys(record)) {
    if (!allowed.has(key)) {
      corruptRuntimeAccount(`${detail} contains unknown field ${key}`);
    }
  }
  return record;
}

function requiredString(
  record: Record<string, unknown>,
  key: string,
  detail: string,
): string {
  const value = record[key];
  if (typeof value !== "string") {
    return corruptRuntimeAccount(`${detail}.${key} is not a string`);
  }
  return value;
}

function requiredInteger(
  record: Record<string, unknown>,
  key: string,
  detail: string,
  minimum = 0,
): number {
  const value = record[key];
  if (
    typeof value !== "number"
    || !Number.isSafeInteger(value)
    || value < minimum
  ) {
    return corruptRuntimeAccount(`${detail}.${key} is not a valid integer`);
  }
  return value;
}

function optionalString(
  record: Record<string, unknown>,
  key: string,
  detail: string,
): string | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== "string") {
    return corruptRuntimeAccount(`${detail}.${key} is not a string`);
  }
  return value;
}

function optionalInteger(
  record: Record<string, unknown>,
  key: string,
  detail: string,
): number | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    return corruptRuntimeAccount(`${detail}.${key} is not a valid integer`);
  }
  return value as number;
}

function decodeStringDictionary(
  value: unknown,
  detail: string,
): Record<string, string> {
  const record = strictObject(
    value,
    value && typeof value === "object" ? Object.keys(value) : [],
    detail,
  );
  const decoded = dictionary<string>();
  for (const [key, entry] of Object.entries(record)) {
    if (typeof entry !== "string") {
      corruptRuntimeAccount(`${detail}.${key} is not a string`);
    }
    decoded[key] = entry;
  }
  return decoded;
}

function decodeMarkup(value: unknown, detail: string): NonNullable<PersistedQueuedMessage["markup"]> {
  if (!Array.isArray(value)) {
    return corruptRuntimeAccount(`${detail} is not an array`);
  }
  return value.map((entry, index) => {
    const label = `${detail}[${index}]`;
    const base = strictObject(
      entry,
      ["type", "start", "end", "styles", "language", "ordered", "items"],
      label,
    );
    const type = requiredString(base, "type", label);
    const start = requiredInteger(base, "start", label);
    const end = requiredInteger(base, "end", label);
    if (end <= start) corruptRuntimeAccount(`${label} has an empty range`);
    if (type === "span") {
      if (!Array.isArray(base.styles) || !base.styles.every((style) => (
        style === "strong"
        || style === "emphasis"
        || style === "deleted"
        || style === "code"
      ))) {
        corruptRuntimeAccount(`${label}.styles is invalid`);
      }
      const exact = strictObject(entry, ["type", "start", "end", "styles"], label);
      return {
        type: "span" as const,
        start,
        end,
        styles: [...exact.styles as Array<"strong" | "emphasis" | "deleted" | "code">],
      };
    }
    if (type === "bcode") {
      const exact = strictObject(entry, ["type", "start", "end", "language"], label);
      const language = optionalString(exact, "language", label);
      return {
        type: "bcode" as const,
        start,
        end,
        ...(language === undefined ? {} : { language }),
      };
    }
    if (type === "bquote") {
      strictObject(entry, ["type", "start", "end"], label);
      return { type: "bquote" as const, start, end };
    }
    if (type === "list") {
      const exact = strictObject(
        entry,
        ["type", "start", "end", "ordered", "items"],
        label,
      );
      if (typeof exact.ordered !== "boolean") {
        corruptRuntimeAccount(`${label}.ordered is invalid`);
      }
      if (
        !Array.isArray(exact.items)
        || !exact.items.every((item) => Number.isSafeInteger(item) && item >= 0)
      ) {
        corruptRuntimeAccount(`${label}.items is invalid`);
      }
      return {
        type: "list" as const,
        start,
        end,
        ordered: exact.ordered,
        items: [...exact.items as number[]],
      };
    }
    return corruptRuntimeAccount(`${label}.type is invalid`);
  });
}

function decodeReferences(value: unknown, detail: string): NonNullable<PersistedQueuedMessage["references"]> {
  if (!Array.isArray(value)) {
    return corruptRuntimeAccount(`${detail} is not an array`);
  }
  return value.map((entry, index) => {
    const label = `${detail}[${index}]`;
    const record = strictObject(
      entry,
      ["type", "uri", "begin", "end", "anchor"],
      label,
    );
    const begin = optionalInteger(record, "begin", label);
    const end = optionalInteger(record, "end", label);
    if ((begin === undefined) !== (end === undefined)) {
      corruptRuntimeAccount(`${label} has a partial range`);
    }
    const anchor = optionalString(record, "anchor", label);
    return {
      type: requiredString(record, "type", label),
      uri: requiredString(record, "uri", label),
      ...(begin === undefined ? {} : { begin, end: end! }),
      ...(anchor === undefined ? {} : { anchor }),
    };
  });
}

function decodeEncryptedFile(value: unknown, detail: string): NonNullable<NonNullable<PersistedQueuedMessage["files"]>[number]["encrypted"]> {
  const record = strictObject(
    value,
    ["cipher", "keyB64", "ivB64", "hashes", "sources"],
    detail,
  );
  const cipher = requiredString(record, "cipher", detail);
  if (
    cipher !== "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"
    && cipher !== "urn:xmpp:ciphers:aes-256-gcm-nopadding:0"
  ) {
    corruptRuntimeAccount(`${detail}.cipher is invalid`);
  }
  let hashes: Array<{ algo: string; valueB64: string }> | undefined;
  if (record.hashes !== undefined) {
    if (!Array.isArray(record.hashes)) {
      corruptRuntimeAccount(`${detail}.hashes is invalid`);
    }
    hashes = record.hashes.map((entry, index) => {
      const hash = strictObject(entry, ["algo", "valueB64"], `${detail}.hashes[${index}]`);
      return {
        algo: requiredString(hash, "algo", detail),
        valueB64: requiredString(hash, "valueB64", detail),
      };
    });
  }
  let sources: string[] | undefined;
  if (record.sources !== undefined) {
    if (!Array.isArray(record.sources) || !record.sources.every((entry) => typeof entry === "string")) {
      corruptRuntimeAccount(`${detail}.sources is invalid`);
    }
    sources = [...record.sources as string[]];
  }
  return {
    cipher,
    keyB64: requiredString(record, "keyB64", detail),
    ivB64: requiredString(record, "ivB64", detail),
    ...(hashes ? { hashes } : {}),
    ...(sources ? { sources } : {}),
  };
}

function decodeFiles(value: unknown, detail: string): NonNullable<PersistedQueuedMessage["files"]> {
  if (!Array.isArray(value)) {
    return corruptRuntimeAccount(`${detail} is not an array`);
  }
  return value.map((entry, index) => {
    const label = `${detail}[${index}]`;
    const record = strictObject(
      entry,
      [
        "url",
        "name",
        "mediaType",
        "size",
        "disposition",
        "width",
        "height",
        "encrypted",
      ],
      label,
    );
    const disposition = requiredString(record, "disposition", label);
    if (disposition !== "inline" && disposition !== "attachment") {
      corruptRuntimeAccount(`${label}.disposition is invalid`);
    }
    const width = optionalInteger(record, "width", label);
    const height = optionalInteger(record, "height", label);
    return {
      url: requiredString(record, "url", label),
      name: requiredString(record, "name", label),
      mediaType: requiredString(record, "mediaType", label),
      size: requiredInteger(record, "size", label),
      disposition,
      ...(width === undefined ? {} : { width }),
      ...(height === undefined ? {} : { height }),
      ...(record.encrypted === undefined
        ? {}
        : { encrypted: decodeEncryptedFile(record.encrypted, `${label}.encrypted`) }),
    };
  });
}

function decodeQueuedMessage(
  value: unknown,
  detail: string,
): PersistedQueuedMessage {
  const commonKeys = [
    "kind",
    "id",
    "createdAt",
    "body",
    "markup",
    "references",
    "mentionJidsByNick",
    "files",
    "replyTo",
    "threadId",
    "parentThreadId",
  ];
  const preliminary = strictObject(
    value,
    [...commonKeys, "roomJid", "threadCreate", "threadReply", "peerJid", "mucPm"],
    detail,
  );
  const kind = requiredString(preliminary, "kind", detail);
  const allowed = kind === "room"
    ? [...commonKeys, "roomJid", "threadCreate", "threadReply"]
    : kind === "dm"
      ? [...commonKeys, "peerJid", "mucPm"]
      : corruptRuntimeAccount(`${detail}.kind is invalid`);
  const record = strictObject(value, allowed, detail);
  const base = {
    id: requiredString(record, "id", detail),
    createdAt: requiredString(record, "createdAt", detail),
    body: requiredString(record, "body", detail),
    ...(record.markup === undefined ? {} : { markup: decodeMarkup(record.markup, `${detail}.markup`) }),
    ...(record.references === undefined ? {} : { references: decodeReferences(record.references, `${detail}.references`) }),
    ...(record.mentionJidsByNick === undefined
      ? {}
      : { mentionJidsByNick: decodeStringDictionary(record.mentionJidsByNick, `${detail}.mentionJidsByNick`) }),
    ...(record.files === undefined ? {} : { files: decodeFiles(record.files, `${detail}.files`) }),
    ...(record.replyTo === undefined
      ? {}
      : {
          replyTo: (() => {
            const reply = strictObject(record.replyTo, ["id", "author", "body"], `${detail}.replyTo`);
            const body = optionalString(reply, "body", `${detail}.replyTo`);
            return {
              id: requiredString(reply, "id", `${detail}.replyTo`),
              author: requiredString(reply, "author", `${detail}.replyTo`),
              ...(body === undefined ? {} : { body }),
            };
          })(),
        }),
    ...(optionalString(record, "threadId", detail) === undefined
      ? {}
      : { threadId: record.threadId as string }),
    ...(optionalString(record, "parentThreadId", detail) === undefined
      ? {}
      : { parentThreadId: record.parentThreadId as string }),
  };
  if (kind === "room") {
    let threadCreate: { title: string } | undefined;
    if (record.threadCreate !== undefined) {
      const create = strictObject(record.threadCreate, ["title"], `${detail}.threadCreate`);
      threadCreate = { title: requiredString(create, "title", `${detail}.threadCreate`) };
    }
    let threadReply: { threadId: string } | undefined;
    if (record.threadReply !== undefined) {
      const reply = strictObject(record.threadReply, ["threadId"], `${detail}.threadReply`);
      threadReply = { threadId: requiredString(reply, "threadId", `${detail}.threadReply`) };
    }
    return {
      kind: "room",
      ...base,
      roomJid: requiredString(record, "roomJid", detail),
      ...(threadCreate ? { threadCreate } : {}),
      ...(threadReply ? { threadReply } : {}),
    };
  }
  if (record.mucPm !== undefined && typeof record.mucPm !== "boolean") {
    corruptRuntimeAccount(`${detail}.mucPm is invalid`);
  }
  return {
    kind: "dm",
    ...base,
    peerJid: requiredString(record, "peerJid", detail),
    ...(record.mucPm === undefined
      ? {}
      : { mucPm: record.mucPm as boolean }),
  };
}

function decodeOwnerFenceFields(
  record: Record<string, unknown>,
  detail: string,
): OutboundOwnerContext {
  return {
    accountKey: requiredString(record, "accountKey", detail),
    ownerId: requiredString(record, "ownerId", detail),
    ownerInstanceId: requiredString(record, "ownerInstanceId", detail),
    ownerGeneration: requiredInteger(record, "ownerGeneration", detail, 1),
    authorityEpoch: requiredInteger(record, "authorityEpoch", detail),
  };
}

function decodeClaim(value: unknown, detail: string): OutboundClaim {
  const record = strictObject(
    value,
    [
      "accountKey",
      "ownerId",
      "ownerInstanceId",
      "ownerGeneration",
      "authorityEpoch",
      "connectionGeneration",
      "claimId",
      "phase",
      "rowIncarnation",
      "payloadDigest",
      "leaseUntil",
    ],
    detail,
  );
  const phase = requiredString(record, "phase", detail);
  if (phase !== "sending" && phase !== "resume-replay" && phase !== "fresh-fallback") {
    corruptRuntimeAccount(`${detail}.phase is invalid`);
  }
  return {
    ...decodeOwnerFenceFields(record, detail),
    connectionGeneration: requiredInteger(record, "connectionGeneration", detail),
    claimId: requiredString(record, "claimId", detail),
    phase,
    rowIncarnation: requiredString(record, "rowIncarnation", detail),
    payloadDigest: requiredString(record, "payloadDigest", detail),
    leaseUntil: requiredInteger(record, "leaseUntil", detail),
  };
}

function decodeIdentity(value: unknown, detail: string): OutboundRowIdentity {
  const record = strictObject(
    value,
    ["accountKey", "messageId", "incarnation", "payloadDigest"],
    detail,
  );
  return {
    accountKey: requiredString(record, "accountKey", detail),
    messageId: requiredString(record, "messageId", detail),
    incarnation: requiredString(record, "incarnation", detail),
    payloadDigest: requiredString(record, "payloadDigest", detail),
  };
}

function decodeLane(value: unknown, detail: string): OutboundLane {
  const preliminary = strictObject(value, ["kind", "roomJid"], detail);
  const kind = requiredString(preliminary, "kind", detail);
  if (kind === "direct") {
    strictObject(value, ["kind"], detail);
    return { kind: "direct" };
  }
  if (kind === "room") {
    const room = strictObject(value, ["kind", "roomJid"], detail);
    return { kind: "room", roomJid: requiredString(room, "roomJid", detail) };
  }
  return corruptRuntimeAccount(`${detail}.kind is invalid`);
}

function decodeOutboundRow(value: unknown, detail: string): DurableOutboundRow {
  const record = strictObject(
    value,
    ["identity", "lane", "orderKey", "message", "state"],
    detail,
  );
  const identity = decodeIdentity(record.identity, `${detail}.identity`);
  const message = decodeQueuedMessage(record.message, `${detail}.message`);
  const lane = decodeLane(record.lane, `${detail}.lane`);
  const stateRecord = strictObject(
    record.state,
    ["kind", "claim", "intentId"],
    `${detail}.state`,
  );
  const stateKind = requiredString(stateRecord, "kind", `${detail}.state`);
  let state: DurableOutboundEntryState;
  if (stateKind === "ready") {
    strictObject(record.state, ["kind"], `${detail}.state`);
    state = { kind: "ready" };
  } else if (stateKind === "claimed") {
    const exact = strictObject(record.state, ["kind", "claim"], `${detail}.state`);
    state = { kind: "claimed", claim: decodeClaim(exact.claim, `${detail}.state.claim`) };
  } else if (stateKind === "terminal") {
    const exact = strictObject(record.state, ["kind", "intentId"], `${detail}.state`);
    state = {
      kind: "terminal",
      intentId: requiredString(exact, "intentId", `${detail}.state`),
    };
  } else {
    return corruptRuntimeAccount(`${detail}.state.kind is invalid`);
  }
  if (
    identity.messageId !== message.id
    || identity.accountKey === ""
    || !sameLane(lane, outboundLane(message))
  ) {
    corruptRuntimeAccount(`${detail} identity/lane does not match its message`);
  }
  const expectedOrderKey = orderKey(message);
  const persistedOrderKey = requiredString(record, "orderKey", detail);
  if (persistedOrderKey !== expectedOrderKey) {
    corruptRuntimeAccount(`${detail}.orderKey is not canonical`);
  }
  if (
    state.kind === "claimed"
    && (
      state.claim.accountKey !== identity.accountKey
      || state.claim.rowIncarnation !== identity.incarnation
      || state.claim.payloadDigest !== identity.payloadDigest
    )
  ) {
    corruptRuntimeAccount(`${detail}.claim does not match row identity`);
  }
  return { identity, lane, orderKey: persistedOrderKey, message, state };
}

function decodeOwner(value: unknown, detail: string): DurableOutboundOwner {
  const record = strictObject(
    value,
    [
      "ownerId",
      "ownerInstanceId",
      "ownerGeneration",
      "authorityEpoch",
      "leaseUntil",
      "lastRenewedAt",
      "handoff",
      "predecessors",
    ],
    detail,
  );
  let handoff: OutboundOwnerHandoff | undefined;
  if (record.handoff !== undefined) {
    const value = strictObject(
      record.handoff,
      ["token", "expiresAt", "authorityEpoch", "ownerGeneration"],
      `${detail}.handoff`,
    );
    handoff = {
      token: requiredString(value, "token", `${detail}.handoff`),
      expiresAt: requiredInteger(value, "expiresAt", `${detail}.handoff`),
      authorityEpoch: requiredInteger(value, "authorityEpoch", `${detail}.handoff`),
      ownerGeneration: requiredInteger(value, "ownerGeneration", `${detail}.handoff`, 1),
    };
  }
  let predecessors: DurablePredecessorFence[] | undefined;
  if (record.predecessors !== undefined) {
    if (
      !Array.isArray(record.predecessors)
      || record.predecessors.length === 0
      || record.predecessors.length > RETAINED_PREDECESSOR_LIMIT
    ) {
      corruptRuntimeAccount(`${detail}.predecessors is not a bounded non-empty array`);
    }
    predecessors = record.predecessors.map((entry, index) => {
      const predecessorDetail = `${detail}.predecessors[${index}]`;
      const value = strictObject(
        entry,
        ["ownerInstanceId", "ownerGeneration", "authorityEpoch", "expiresAt"],
        predecessorDetail,
      );
      return {
        ownerInstanceId: requiredString(value, "ownerInstanceId", predecessorDetail),
        ownerGeneration: requiredInteger(value, "ownerGeneration", predecessorDetail, 1),
        authorityEpoch: requiredInteger(value, "authorityEpoch", predecessorDetail),
        expiresAt: requiredInteger(value, "expiresAt", predecessorDetail),
      };
    });
  }
  return {
    ownerId: requiredString(record, "ownerId", detail),
    ownerInstanceId: requiredString(record, "ownerInstanceId", detail),
    ownerGeneration: requiredInteger(record, "ownerGeneration", detail, 1),
    authorityEpoch: requiredInteger(record, "authorityEpoch", detail),
    leaseUntil: requiredInteger(record, "leaseUntil", detail),
    lastRenewedAt: requiredInteger(record, "lastRenewedAt", detail),
    ...(handoff ? { handoff } : {}),
    ...(predecessors ? { predecessors } : {}),
  };
}

function decodeSmRecord(value: unknown, detail: string): DurableSmRecord {
  const record = strictObject(
    value,
    [
      "accountKey",
      "ownerId",
      "ownerGeneration",
      "authorityEpoch",
      "version",
      "state",
      "savedAt",
      "consumed",
    ],
    detail,
  );
  if (record.state !== null && record.state === undefined) {
    corruptRuntimeAccount(`${detail}.state is missing`);
  }
  if (typeof record.consumed !== "boolean") {
    corruptRuntimeAccount(`${detail}.consumed is invalid`);
  }
  return {
    accountKey: requiredString(record, "accountKey", detail),
    ownerId: requiredString(record, "ownerId", detail),
    ownerGeneration: requiredInteger(record, "ownerGeneration", detail, 1),
    authorityEpoch: requiredInteger(record, "authorityEpoch", detail),
    version: requiredInteger(record, "version", detail),
    state: record.state === null
      ? null
      : decodePersistedSmResumeState(record.state, `${detail}.state`),
    savedAt: requiredInteger(record, "savedAt", detail),
    consumed: record.consumed,
  };
}

function decodeTerminalIntent(
  value: unknown,
  detail: string,
): OutboundTerminalIntent {
  const record = strictObject(
    value,
    ["intentId", "accountKey", "identity", "kind", "expected", "recordedAt"],
    detail,
  );
  const kind = requiredString(record, "kind", detail);
  if (kind !== "ack" && kind !== "native-failure" && kind !== "nonretryable-delete") {
    corruptRuntimeAccount(`${detail}.kind is invalid`);
  }
  const identity = decodeIdentity(record.identity, `${detail}.identity`);
  const expected = decodeClaim(record.expected, `${detail}.expected`);
  if (!claimMatchesIdentity(expected, identity)) {
    corruptRuntimeAccount(`${detail}.expected does not match its row identity`);
  }
  return {
    intentId: requiredString(record, "intentId", detail),
    accountKey: requiredString(record, "accountKey", detail),
    identity,
    kind,
    expected,
    recordedAt: requiredInteger(record, "recordedAt", detail),
  };
}

function decodeDictionary<T>(
  value: unknown,
  detail: string,
  decode: (entry: unknown, entryDetail: string) => T,
): Record<string, T> {
  const raw = strictObject(
    value,
    value && typeof value === "object" ? Object.keys(value) : [],
    detail,
  );
  const result = dictionary<T>();
  for (const [key, entry] of Object.entries(raw)) {
    result[key] = decode(entry, `${detail}.${key}`);
  }
  return result;
}

function decodeRuntimeAccount(
  value: unknown,
  expectedAccountKey: string,
): RuntimeAccount {
  const record = strictObject(
    value,
    [
      "accountKey",
      "schemaVersion",
      "revision",
      "lastAuthorityTimeMs",
      "lastWallClockSampleMs",
      "authorityEpoch",
      "nextOwnerGeneration",
      "owners",
      "outbound",
      "terminals",
      "smSnapshots",
    ],
    "account",
  );
  const accountKey = requiredString(record, "accountKey", "account");
  if (accountKey !== expectedAccountKey) {
    corruptRuntimeAccount("key does not match requested account");
  }
  if (record.schemaVersion !== 1) {
    corruptRuntimeAccount("schemaVersion is unsupported");
  }
  const account: RuntimeAccount = {
    accountKey,
    schemaVersion: 1,
    revision: requiredInteger(record, "revision", "account"),
    lastAuthorityTimeMs: requiredInteger(record, "lastAuthorityTimeMs", "account"),
    lastWallClockSampleMs: requiredInteger(record, "lastWallClockSampleMs", "account"),
    authorityEpoch: requiredInteger(record, "authorityEpoch", "account"),
    nextOwnerGeneration: requiredInteger(record, "nextOwnerGeneration", "account", 1),
    owners: decodeDictionary(record.owners, "account.owners", decodeOwner),
    outbound: decodeDictionary(record.outbound, "account.outbound", decodeOutboundRow),
    terminals: decodeDictionary(record.terminals, "account.terminals", decodeTerminalIntent),
    smSnapshots: decodeDictionary(record.smSnapshots, "account.smSnapshots", decodeSmRecord),
  };
  for (const [ownerId, owner] of Object.entries(account.owners)) {
    if (ownerId !== owner.ownerId) {
      corruptRuntimeAccount(`owner dictionary key ${ownerId} does not match identity`);
    }
    if (
      owner.authorityEpoch > account.authorityEpoch
      || owner.lastRenewedAt > owner.leaseUntil
    ) {
      corruptRuntimeAccount(`owner ${ownerId} has an invalid authority fence`);
    }
    if (
      owner.handoff
      && (
        owner.handoff.ownerGeneration !== owner.ownerGeneration
        || owner.handoff.authorityEpoch !== owner.authorityEpoch
        || owner.handoff.expiresAt > owner.leaseUntil
      )
    ) {
      corruptRuntimeAccount(`owner ${ownerId} has an invalid handoff fence`);
    }
    let previousGeneration = 0;
    let previousAuthorityEpoch = 0;
    const predecessorFences = new Set<string>();
    for (const predecessor of owner.predecessors ?? []) {
      const exactFence = [
        predecessor.ownerInstanceId,
        predecessor.ownerGeneration,
        predecessor.authorityEpoch,
      ].join("\u0000");
      if (
        predecessor.ownerGeneration <= previousGeneration
        || predecessor.ownerGeneration >= owner.ownerGeneration
        || predecessor.authorityEpoch < previousAuthorityEpoch
        || predecessor.authorityEpoch > owner.authorityEpoch
        || predecessorFences.has(exactFence)
      ) {
        corruptRuntimeAccount(`owner ${ownerId} has an invalid predecessor chain`);
      }
      predecessorFences.add(exactFence);
      previousGeneration = predecessor.ownerGeneration;
      previousAuthorityEpoch = predecessor.authorityEpoch;
    }
  }
  const fenceBelongsToOwner = (
    fence: OutboundOwnerFence,
  ): boolean => {
    const owner = account.owners[fence.ownerId];
    if (!owner || fence.accountKey !== accountKey) return false;
    if (sameOwner(owner, fence)) return true;
    return (owner.predecessors ?? []).some((predecessor) => (
      fence.ownerInstanceId === predecessor.ownerInstanceId
      && fence.ownerGeneration === predecessor.ownerGeneration
      && fence.authorityEpoch === predecessor.authorityEpoch
    ));
  };
  const referencedGenerations: number[] = [];
  for (const owner of Object.values(account.owners)) {
    referencedGenerations.push(owner.ownerGeneration);
    if (owner.handoff) referencedGenerations.push(owner.handoff.ownerGeneration);
    for (const predecessor of owner.predecessors ?? []) {
      referencedGenerations.push(predecessor.ownerGeneration);
    }
  }
  for (const [messageId, row] of Object.entries(account.outbound)) {
    if (
      messageId !== row.identity.messageId
      || row.identity.accountKey !== accountKey
    ) {
      corruptRuntimeAccount(`outbound dictionary key ${messageId} does not match identity`);
    }
    if (row.state.kind === "terminal") {
      const intent = account.terminals[row.state.intentId];
      if (
        !intent
        || intent.identity.messageId !== messageId
        || !sameIdentity(intent.identity, row.identity)
      ) {
        corruptRuntimeAccount(`terminal row ${messageId} has no exact intent`);
      }
    } else if (row.state.kind === "claimed") {
      referencedGenerations.push(row.state.claim.ownerGeneration);
      if (
        row.state.claim.authorityEpoch > account.authorityEpoch
        || !fenceBelongsToOwner(row.state.claim)
      ) {
        corruptRuntimeAccount(`claimed row ${messageId} has an invalid owner fence`);
      }
    }
  }
  for (const [intentId, intent] of Object.entries(account.terminals)) {
    const row = account.outbound[intent.identity.messageId];
    if (
      intentId !== intent.intentId
      || intent.accountKey !== accountKey
      || intent.identity.accountKey !== accountKey
      || !row
      || row.state.kind !== "terminal"
      || row.state.intentId !== intentId
      || !sameIdentity(row.identity, intent.identity)
    ) {
      corruptRuntimeAccount(`intent ${intentId} does not have one exact row`);
    }
    referencedGenerations.push(intent.expected.ownerGeneration);
    if (
      intent.expected.authorityEpoch > account.authorityEpoch
      || !fenceBelongsToOwner(intent.expected)
    ) {
      corruptRuntimeAccount(`intent ${intentId} has an invalid historical fence`);
    }
  }
  for (const [ownerId, snapshot] of Object.entries(account.smSnapshots)) {
    if (
      ownerId !== snapshot.ownerId
      || snapshot.accountKey !== accountKey
    ) {
      corruptRuntimeAccount(`SM snapshot ${ownerId} does not match its key`);
    }
    referencedGenerations.push(snapshot.ownerGeneration);
    if (snapshot.authorityEpoch > account.authorityEpoch) {
      corruptRuntimeAccount(`SM snapshot ${ownerId} has a future authority epoch`);
    }
    const owner = account.owners[ownerId];
    if (
      !owner
      || (
        snapshot.ownerGeneration !== owner.ownerGeneration
        || snapshot.authorityEpoch !== owner.authorityEpoch
      )
    ) {
      corruptRuntimeAccount(`SM snapshot ${ownerId} does not match its live owner`);
    }
  }
  if (
    referencedGenerations.some(
      (generation) => generation >= account.nextOwnerGeneration,
    )
  ) {
    corruptRuntimeAccount("nextOwnerGeneration does not dominate every durable fence");
  }
  return account;
}

/**
 * Exercises the exact IndexedDB decode boundary without exposing the decoded
 * mutable account graph. Tests use this to prove malformed durable state fails
 * closed instead of being repaired in place.
 */
export function validatePersistedRuntimeAccount(
  value: unknown,
  expectedAccountKey: string,
): void {
  decodeRuntimeAccount(value, expectedAccountKey);
}

function classifyFailure(cause: unknown): DurableFailureReason {
  if (cause instanceof DurablePredecessorCapacityError) return "capacity";
  const name = cause instanceof DOMException || cause instanceof Error ? cause.name : "";
  if (name === "QuotaExceededError") return "quota";
  if (name === "SecurityError") return "security";
  if (name === "AbortError" || name === "TransactionInactiveError") return "aborted";
  return "unavailable";
}

function failed<T>(cause: unknown): DurableOutcome<T> {
  return { kind: "failed", reason: classifyFailure(cause), cause };
}

function cloneValue<T>(value: T): T {
  return structuredClone(value);
}

function entryFromRow(row: DurableOutboundRow): DurableOutboundEntry {
  return {
    identity: { ...row.identity },
    lane: { ...row.lane },
    message: cloneValue(row.message),
    state: cloneValue(row.state),
  };
}

function smEnvelope(record: DurableSmRecord): DurableSmEnvelope | null {
  if (!record.state) return null;
  return {
    accountKey: record.accountKey,
    ownerId: record.ownerId,
    ownerGeneration: record.ownerGeneration,
    authorityEpoch: record.authorityEpoch,
    version: record.version,
    state: cloneSmResumeState(record.state),
    savedAt: record.savedAt,
    consumed: record.consumed,
  };
}

function sameSmFence(
  record: DurableSmRecord | undefined,
  owner: OutboundOwnerContext,
): record is DurableSmRecord {
  return !!record
    && record.accountKey === owner.accountKey
    && record.ownerId === owner.ownerId
    && record.ownerGeneration === owner.ownerGeneration
    && record.authorityEpoch === owner.authorityEpoch;
}

function ownerFence(
  accountKey: string,
  owner: DurableOutboundOwner,
): OutboundOwnerContext {
  return {
    accountKey,
    ownerId: owner.ownerId,
    ownerInstanceId: owner.ownerInstanceId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
  };
}

function sameOwner(
  persisted: DurableOutboundOwner | undefined,
  expected: OutboundOwnerContext,
): boolean {
  return !!persisted
    && persisted.ownerId === expected.ownerId
    && persisted.ownerInstanceId === expected.ownerInstanceId
    && persisted.ownerGeneration === expected.ownerGeneration
    && persisted.authorityEpoch === expected.authorityEpoch;
}

function currentOwner(
  account: RuntimeAccount,
  expected: OutboundOwnerContext,
  now: number,
): DurableOutboundOwner | null {
  if (expected.accountKey !== account.accountKey) return null;
  if (expected.authorityEpoch !== account.authorityEpoch) return null;
  const persisted = account.owners[expected.ownerId];
  return sameOwner(persisted, expected) && persisted.leaseUntil > now
    ? persisted
    : null;
}

function allocateOwnerGeneration(account: RuntimeAccount): number {
  const generation = account.nextOwnerGeneration;
  if (!Number.isSafeInteger(generation) || generation <= 0) {
    throw new DOMException("Outbound owner generation exhausted", "AbortError");
  }
  account.nextOwnerGeneration = checkedDurableCounterIncrement(
    generation,
    "Outbound owner generation",
  );
  return generation;
}

function sameIdentity(
  left: OutboundRowIdentity,
  right: OutboundRowIdentity,
): boolean {
  return left.accountKey === right.accountKey
    && left.messageId === right.messageId
    && left.incarnation === right.incarnation
    && left.payloadDigest === right.payloadDigest;
}

function sameClaim(
  left: OutboundClaim | undefined,
  right: OutboundClaim,
): boolean {
  return !!left
    && left.accountKey === right.accountKey
    && left.ownerId === right.ownerId
    && left.ownerInstanceId === right.ownerInstanceId
    && left.ownerGeneration === right.ownerGeneration
    && left.authorityEpoch === right.authorityEpoch
    && left.connectionGeneration === right.connectionGeneration
    && left.claimId === right.claimId
    && left.phase === right.phase
    && left.rowIncarnation === right.rowIncarnation
    && left.payloadDigest === right.payloadDigest;
}

function claimMatchesIdentity(
  claim: Pick<OutboundClaim, "accountKey" | "rowIncarnation" | "payloadDigest">,
  identity: OutboundRowIdentity,
): boolean {
  return claim.accountKey === identity.accountKey
    && claim.rowIncarnation === identity.incarnation
    && claim.payloadDigest === identity.payloadDigest;
}

function claimForRow(
  request: OutboundClaimRequest,
  identity: OutboundRowIdentity,
  authorityNow: number,
): OutboundClaim {
  return {
    ...request,
    rowIncarnation: identity.incarnation,
    payloadDigest: identity.payloadDigest,
    leaseUntil: checkedDurableDeadline(
      authorityNow,
      OUTBOUND_CLAIM_LEASE_MS,
      "Outbound claim lease",
    ),
  };
}

function orderKey(message: PersistedQueuedMessage): string {
  return `${message.createdAt}\u0000${message.id}`;
}

export function outboundLane(message: PersistedQueuedMessage): OutboundLane {
  return message.kind === "dm"
    ? { kind: "direct" }
    : { kind: "room", roomJid: message.roomJid };
}

function sameLane(left: OutboundLane, right: OutboundLane): boolean {
  return left.kind === right.kind
    && (left.kind === "direct"
      || (right.kind === "room" && left.roomJid === right.roomJid));
}

function orderedRows(account: RuntimeAccount): DurableOutboundRow[] {
  return Object.values(account.outbound).sort((left, right) => (
    left.orderKey.localeCompare(right.orderKey)
  ));
}

function claimReferencesOwner(
  claim: OutboundClaim,
  owner: DurableOutboundOwner,
): boolean {
  return claim.ownerId === owner.ownerId
    && claim.ownerInstanceId === owner.ownerInstanceId
    && claim.ownerGeneration === owner.ownerGeneration
    && claim.authorityEpoch === owner.authorityEpoch;
}

function claimReferencesPredecessor(
  claim: OutboundClaim,
  owner: DurableOutboundOwner,
): boolean {
  return (owner.predecessors ?? []).some((predecessor) => (
    claim.ownerId === owner.ownerId
    && claim.ownerInstanceId === predecessor.ownerInstanceId
    && claim.ownerGeneration === predecessor.ownerGeneration
    && claim.authorityEpoch === predecessor.authorityEpoch
  ));
}

function predecessorHasDurableReference(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
  predecessor: DurablePredecessorFence,
): boolean {
  const matches = (claim: OutboundClaim) => (
    claim.ownerId === owner.ownerId
    && claim.ownerInstanceId === predecessor.ownerInstanceId
    && claim.ownerGeneration === predecessor.ownerGeneration
    && claim.authorityEpoch === predecessor.authorityEpoch
  );
  return Object.values(account.outbound).some((row) => (
    row.state.kind === "claimed"
    && matches(row.state.claim)
  )) || Object.values(account.terminals).some((intent) => (
    matches(intent.expected)
  ));
}

function pruneUnreferencedPredecessors(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
): boolean {
  const existing = owner.predecessors ?? [];
  const retained = existing.filter((predecessor) => (
    predecessorHasDurableReference(account, owner, predecessor)
  ));
  if (retained.length === existing.length) return false;
  if (retained.length === 0) {
    delete owner.predecessors;
  } else {
    owner.predecessors = retained;
  }
  return true;
}

function retainedPredecessorsForHandoff(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
): DurablePredecessorFence[] {
  const retained = (owner.predecessors ?? []).filter((predecessor) => (
    predecessorHasDurableReference(account, owner, predecessor)
  ));
  const currentIsReferenced = Object.values(account.outbound).some((row) => (
    row.state.kind === "claimed"
    && claimReferencesOwner(row.state.claim, owner)
  )) || Object.values(account.terminals).some((intent) => (
    claimReferencesOwner(intent.expected, owner)
  ));
  if (currentIsReferenced) {
    retained.push({
      ownerInstanceId: owner.ownerInstanceId,
      ownerGeneration: owner.ownerGeneration,
      authorityEpoch: owner.authorityEpoch,
      expiresAt: owner.handoff?.expiresAt ?? owner.leaseUntil,
    });
  }
  if (retained.length > RETAINED_PREDECESSOR_LIMIT) {
    throw new DurablePredecessorCapacityError();
  }
  return retained;
}

function smRecordRetentionDeadline(record: DurableSmRecord): number {
  if (!record.state || record.consumed) {
    return checkedDurableDeadline(
      record.savedAt,
      SM_SNAPSHOT_RETENTION_MS,
      "Durable SM record retention",
    );
  }
  const configuredSeconds = record.state.maxResumeSeconds;
  const resumeWindowMs = typeof configuredSeconds === "number"
    && Number.isInteger(configuredSeconds)
    && configuredSeconds > 0
    ? configuredSeconds * 1_000
    : DEFAULT_SM_RESUME_WINDOW_MS;
  const resumeDeadline = checkedDurableDeadline(
    record.savedAt,
    resumeWindowMs,
    "Durable SM resume window",
  );
  return checkedDurableDeadline(
    resumeDeadline,
    SM_SNAPSHOT_RETENTION_MS,
    "Durable SM record retention",
  );
}

function ownerHasDurableReference(
  account: RuntimeAccount,
  owner: DurableOutboundOwner,
  authorityNow: number,
): boolean {
  if (
    owner.handoff
    && owner.handoff.expiresAt > authorityNow
    && owner.handoff.ownerGeneration === owner.ownerGeneration
    && owner.handoff.authorityEpoch === owner.authorityEpoch
  ) return true;
  if (Object.values(account.outbound).some((row) => (
    row.state.kind === "claimed"
    && (
      claimReferencesOwner(row.state.claim, owner)
      || claimReferencesPredecessor(row.state.claim, owner)
    )
  ))) return true;
  if (Object.values(account.terminals).some((intent) => (
    claimReferencesOwner(intent.expected, owner)
    || claimReferencesPredecessor(intent.expected, owner)
  ))) return true;
  const snapshot = account.smSnapshots[owner.ownerId];
  return !!snapshot
    && snapshot.ownerGeneration === owner.ownerGeneration
    && snapshot.authorityEpoch === owner.authorityEpoch;
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("Outbound payload contains a non-finite number");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    const fields = Object.keys(record)
      .filter((key) => record[key] !== undefined)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`);
    return `{${fields.join(",")}}`;
  }
  throw new TypeError("Outbound payload contains an unsupported value");
}

async function outboundPayloadDigest(
  message: PersistedQueuedMessage,
): Promise<string> {
  const { id: _id, createdAt: _createdAt, ...semantics } = message;
  if (!globalThis.crypto?.subtle) {
    throw new DOMException("WebCrypto digest is unavailable", "NotSupportedError");
  }
  const bytes = new TextEncoder().encode(canonicalJson(semantics));
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)]
    .map((value) => value.toString(16).padStart(2, "0"))
    .join("");
}

export function createOutboundClaim(
  owner: OutboundOwnerContext,
  connectionGeneration: number,
  phase: OutboundClaimPhase,
): OutboundClaimRequest {
  return {
    accountKey: owner.accountKey,
    ownerId: owner.ownerId,
    ownerInstanceId: owner.ownerInstanceId,
    ownerGeneration: owner.ownerGeneration,
    authorityEpoch: owner.authorityEpoch,
    connectionGeneration,
    claimId: crypto.randomUUID(),
    phase,
  };
}

export type DurableAuthorityClock = {
  now(): number;
};

const systemAuthorityClock: DurableAuthorityClock = {
  now: () => Date.now(),
};

abstract class RuntimeDurableStore implements DurableOutboundStore {
  protected constructor(
    private readonly authorityClock: DurableAuthorityClock = systemAuthorityClock,
  ) {}

  protected abstract transact<T>(
    accountKey: string,
    mutate: (account: RuntimeAccount, authorityNow: number) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>>;

  protected sampleAuthorityTime(account: RuntimeAccount): {
    authorityNow: number;
    metadataChanged: boolean;
    authorityEpochChanged: boolean;
  } {
    const sampled = this.authorityClock.now();
    if (!Number.isSafeInteger(sampled) || sampled < 0) {
      throw new DOMException("Durable authority clock is invalid", "AbortError");
    }
    const wallClockNow = sampled;
    const previousWallClock = account.lastWallClockSampleMs;
    const rolledBack = previousWallClock > 0
      && previousWallClock > AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS
      && wallClockNow
        < previousWallClock - AUTHORITY_CLOCK_ROLLBACK_TOLERANCE_MS;
    if (rolledBack) {
      account.authorityEpoch = checkedDurableCounterIncrement(
        account.authorityEpoch,
        "Durable authority epoch",
      );
    }
    account.lastWallClockSampleMs = wallClockNow;
    return {
      authorityNow: Math.max(wallClockNow, account.lastAuthorityTimeMs),
      metadataChanged: rolledBack || wallClockNow !== previousWallClock,
      authorityEpochChanged: rolledBack,
    };
  }

  revision(accountKey: string): Promise<DurableOutcome<number>> {
    return this.transact(accountKey, (account) => ({
      changed: false,
      value: account.revision,
      finalize: (committedRevision) => committedRevision,
    }));
  }

  list(accountKey: string): Promise<DurableOutcome<PersistedQueuedMessage[]>> {
    return this.transact(accountKey, (account) => ({
      changed: false,
      value: orderedRows(account).map((row) => cloneValue(row.message)),
    }));
  }

  scanAndPrune(
    accountKey: string,
    cutoff: number,
  ): Promise<DurableOutcome<DurableOutboundScan>> {
    return this.transact<DurableOutboundScan>(accountKey, (account, authorityNow) => {
      const pruned: OutboundRowIdentity[] = [];
      let metadataPruned = false;
      for (const [messageId, row] of Object.entries(account.outbound)) {
        const createdAt = Date.parse(row.message.createdAt);
        if (!Number.isFinite(createdAt) || createdAt >= cutoff) continue;
        if (row.state.kind === "terminal") continue;
        if (
          row.state.kind === "claimed"
          && row.state.claim.leaseUntil > authorityNow
        ) continue;
        pruned.push({ ...row.identity });
        delete account.outbound[messageId];
      }
      for (const [ownerId, record] of Object.entries(account.smSnapshots)) {
        const exactOwner = account.owners[ownerId];
        if (
          exactOwner
          && exactOwner.ownerGeneration === record.ownerGeneration
          && exactOwner.authorityEpoch === record.authorityEpoch
          && exactOwner.leaseUntil > authorityNow
        ) continue;
        if (authorityNow <= smRecordRetentionDeadline(record)) continue;
        delete account.smSnapshots[ownerId];
        metadataPruned = true;
      }
      for (const [ownerId, owner] of Object.entries(account.owners)) {
        if (pruneUnreferencedPredecessors(account, owner)) {
          metadataPruned = true;
        }
        if (owner.leaseUntil > authorityNow) continue;
        const lastActiveAt = owner.lastRenewedAt
          ?? Math.max(0, owner.leaseUntil - OUTBOUND_CLAIM_LEASE_MS);
        if (
          authorityNow <= checkedDurableDeadline(
            lastActiveAt,
            OUTBOUND_OWNER_RETENTION_MS,
            "Outbound owner retention",
          )
        ) continue;
        if (ownerHasDurableReference(account, owner, authorityNow)) continue;
        delete account.owners[ownerId];
        metadataPruned = true;
      }
      const value = {
        entries: orderedRows(account).map(entryFromRow),
        pruned,
        revision: account.revision,
      };
      return {
        changed: pruned.length > 0 || metadataPruned,
        value,
        finalize: (committedRevision) => ({
          ...value,
          revision: committedRevision,
        }),
      };
    });
  }

  async persistReady(
    accountKey: string,
    message: PersistedQueuedMessage,
  ): Promise<DurableOutcome<OutboundPersistResult>> {
    let identity: OutboundRowIdentity;
    try {
      identity = {
        accountKey,
        messageId: message.id,
        incarnation: crypto.randomUUID(),
        payloadDigest: await outboundPayloadDigest(message),
      };
    } catch (error) {
      return failed(error);
    }
    return this.transact<OutboundPersistResult>(accountKey, (account) => {
      const existing = account.outbound[message.id];
      if (existing) {
        if (existing.identity.payloadDigest !== identity.payloadDigest) {
          return {
            changed: false,
            value: {
              kind: "conflict",
              messageId: message.id,
              existingPayloadDigest: existing.identity.payloadDigest,
              attemptedPayloadDigest: identity.payloadDigest,
            },
          };
        }
        return {
          changed: false,
          value: { kind: "existing", entry: entryFromRow(existing) },
        };
      }
      const row: DurableOutboundRow = {
        identity,
        lane: outboundLane(message),
        orderKey: orderKey(message),
        message: cloneValue(message),
        state: { kind: "ready" },
      };
      account.outbound[message.id] = row;
      return {
        changed: true,
        value: { kind: "inserted", entry: entryFromRow(row) },
      };
    });
  }

  async persistClaimed(
    accountKey: string,
    message: PersistedQueuedMessage,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundPersistClaimedResult>> {
    let identity: OutboundRowIdentity;
    try {
      identity = {
        accountKey,
        messageId: message.id,
        incarnation: crypto.randomUUID(),
        payloadDigest: await outboundPayloadDigest(message),
      };
    } catch (error) {
      return failed(error);
    }
    return this.transact<OutboundPersistClaimedResult>(
      accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, request, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const existing = account.outbound[message.id];
      if (existing) {
        if (existing.identity.payloadDigest !== identity.payloadDigest) {
          return {
            changed: false,
            value: {
              kind: "conflict",
              messageId: message.id,
              existingPayloadDigest: existing.identity.payloadDigest,
              attemptedPayloadDigest: identity.payloadDigest,
            },
          };
        }
        if (existing.state.kind === "terminal") {
          return {
            changed: false,
            value: { kind: "terminal", entry: entryFromRow(existing) },
          };
        }
        if (
          existing.state.kind === "claimed"
          && existing.state.claim.leaseUntil > authorityNow
        ) {
          return {
            changed: false,
            value: {
              kind: "busy",
              entry: entryFromRow(existing),
              leaseUntil: existing.state.claim.leaseUntil,
            },
          };
        }
        const claim = claimForRow(request, existing.identity, authorityNow);
        existing.state = { kind: "claimed", claim };
        return {
          changed: true,
          value: { kind: "claimed", entry: entryFromRow(existing), claim: { ...claim } },
        };
      }
      const claim = claimForRow(request, identity, authorityNow);
      const row: DurableOutboundRow = {
        identity,
        lane: outboundLane(message),
        orderKey: orderKey(message),
        message: cloneValue(message),
        state: { kind: "claimed", claim },
      };
      account.outbound[message.id] = row;
      return {
        changed: true,
        value: { kind: "claimed", entry: entryFromRow(row), claim: { ...claim } },
      };
      },
    );
  }

  claimHead(
    accountKey: string,
    lane: OutboundLane,
    request: OutboundClaimRequest,
  ): Promise<DurableOutcome<OutboundClaimHeadResult>> {
    return this.transact<OutboundClaimHeadResult>(
      accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, request, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const head = orderedRows(account).find((row) => sameLane(row.lane, lane));
      if (!head) return { changed: false, value: { kind: "missing" } };
      if (head.state.kind === "terminal") {
        return {
          changed: false,
          value: { kind: "terminal", messageId: head.identity.messageId },
        };
      }
      if (
        head.state.kind === "claimed"
        && head.state.claim.leaseUntil > authorityNow
      ) {
        return {
          changed: false,
          value: {
            kind: "busy",
            messageId: head.identity.messageId,
            leaseUntil: head.state.claim.leaseUntil,
          },
        };
      }
      const claim = claimForRow(request, head.identity, authorityNow);
      head.state = { kind: "claimed", claim };
      return {
        changed: true,
        value: { kind: "claimed", entry: entryFromRow(head), claim: { ...claim } },
      };
      },
    );
  }

  renew(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundRenewResult>> {
    return this.transact<OutboundRenewResult>(identity.accountKey, (account, authorityNow) => {
      if (!currentOwner(account, expected, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const row = account.outbound[identity.messageId];
      if (
        !row
        || !sameIdentity(row.identity, identity)
        || row.state.kind !== "claimed"
        || !sameClaim(row.state.claim, expected)
      ) {
        return { changed: false, value: { kind: "missing" } };
      }
      if (row.state.claim.leaseUntil <= authorityNow) {
        return { changed: false, value: { kind: "missing" } };
      }
      const claim = {
        ...row.state.claim,
        leaseUntil: checkedDurableDeadline(
          authorityNow,
          OUTBOUND_CLAIM_LEASE_MS,
          "Outbound claim lease",
        ),
      };
      row.state = { kind: "claimed", claim };
      return {
        changed: true,
        value: { kind: "renewed", claim: { ...claim } },
      };
    });
  }

  release(
    identity: OutboundRowIdentity,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundReleaseResult>> {
    return this.transact<OutboundReleaseResult>(identity.accountKey, (account, authorityNow) => {
      if (!currentOwner(account, expected, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const row = account.outbound[identity.messageId];
      if (
        !row
        || !sameIdentity(row.identity, identity)
        || row.state.kind !== "claimed"
        || !sameClaim(row.state.claim, expected)
      ) {
        return { changed: false, value: { kind: "missing" } };
      }
      row.state = { kind: "ready" };
      return { changed: true, value: { kind: "released" } };
    });
  }

  async reconcileResumeClaims(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
    authoritativeMessageIds: readonly string[] | null,
    phase: Extract<OutboundClaimPhase, "resume-replay" | "fresh-fallback">,
  ): Promise<DurableOutcome<ResumeClaimReconciliation>> {
    const authoritative = authoritativeMessageIds === null
      ? null
      : new Set(authoritativeMessageIds);
    const claimIds = new Map(
      (authoritativeMessageIds ?? []).map((messageId) => [messageId, crypto.randomUUID()]),
    );
    return this.transact<ResumeClaimReconciliation>(
      owner.accountKey,
      (account, authorityNow) => {
      const persistedOwner = currentOwner(account, owner, authorityNow);
      if (!persistedOwner) {
        return { changed: false, value: { kind: "fenced" } };
      }

      const claims: Array<{ messageId: string; claim: OutboundClaim }> = [];
      const plannedClaims: Array<{
        messageId: string;
        claim: OutboundClaim;
      }> = [];
      const plannedReleases: string[] = [];
      const blockedIds: string[] = [];
      const terminalIds: string[] = [];
      const seenIds = new Set<string>();
      for (const row of orderedRows(account)) {
        const messageId = row.identity.messageId;
        const isAuthoritative = authoritative?.has(messageId) ?? false;
        if (isAuthoritative) seenIds.add(messageId);
        if (row.state.kind === "terminal") {
          if (isAuthoritative) terminalIds.push(messageId);
          continue;
        }
        const existing = row.state.kind === "claimed" ? row.state.claim : null;
        const exactOwner = !!existing
          && existing.ownerId === owner.ownerId
          && existing.ownerInstanceId === owner.ownerInstanceId
          && existing.ownerGeneration === owner.ownerGeneration
          && existing.authorityEpoch === owner.authorityEpoch;
        const predecessorOwned = !!existing
          && claimReferencesPredecessor(existing, persistedOwner);
        const expired = !!existing && existing.leaseUntil <= authorityNow;

        if (isAuthoritative) {
          if (existing && !exactOwner && !predecessorOwned && !expired) {
            blockedIds.push(messageId);
            continue;
          }
          if (
            existing
            && exactOwner
            && existing.connectionGeneration === connectionGeneration
            && existing.phase === phase
          ) {
            claims.push({ messageId, claim: { ...existing } });
            continue;
          }
          const claim = claimForRow({
            ...owner,
            connectionGeneration,
            claimId: claimIds.get(messageId)!,
            phase,
          }, row.identity, authorityNow);
          claims.push({ messageId, claim: { ...claim } });
          plannedClaims.push({ messageId, claim });
          continue;
        }

        if (!existing || (!exactOwner && !predecessorOwned)) continue;
        const preserveCurrentFallback = authoritative !== null
          && exactOwner
          && existing.connectionGeneration === connectionGeneration
          && existing.phase === "fresh-fallback";
        if (preserveCurrentFallback) continue;
        plannedReleases.push(messageId);
      }
      const missingIds = authoritative === null
        ? []
        : [...authoritative].filter((messageId) => !seenIds.has(messageId));

      if (blockedIds.length > 0 || terminalIds.length > 0 || missingIds.length > 0) {
        // This transaction operates on an isolated account value (IDB's
        // structured clone, or the memory adapter's clone). Returning
        // unchanged discards any tentative adoptions/releases above, so an
        // unresolved native snapshot can never partially transfer authority.
        return {
          changed: false,
          value: {
            kind: "reconciled",
            claims: [],
            releasedIds: [],
            blockedIds,
            terminalIds,
            missingIds,
          },
        };
      }

      // Only after the complete snapshot has passed validation do we apply
      // the off-side plan to the transaction clone. A clock/epoch metadata
      // write can therefore never persist partial claim adoption/release.
      for (const { messageId, claim } of plannedClaims) {
        const row = account.outbound[messageId];
        if (!row) {
          throw new DOMException(
            "Validated outbound row disappeared inside one transaction",
            "AbortError",
          );
        }
        row.state = { kind: "claimed", claim };
      }
      for (const messageId of plannedReleases) {
        const row = account.outbound[messageId];
        if (!row) {
          throw new DOMException(
            "Validated outbound row disappeared inside one transaction",
            "AbortError",
          );
        }
        row.state = { kind: "ready" };
      }
      let changed = plannedClaims.length > 0 || plannedReleases.length > 0;
      if (pruneUnreferencedPredecessors(account, persistedOwner)) {
        changed = true;
      }
      return {
        changed,
        value: {
          kind: "reconciled",
          claims,
          releasedIds: plannedReleases,
          blockedIds,
          terminalIds,
          missingIds,
        },
      };
      },
    );
  }

  releaseForFreshSession(
    owner: OutboundOwnerContext,
    connectionGeneration: number,
  ): Promise<DurableOutcome<string[] | null>> {
    return this.transact(owner.accountKey, (account, authorityNow) => {
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: null };
      }
      const released: string[] = [];
      for (const row of orderedRows(account)) {
        if (row.state.kind !== "claimed") continue;
        const claim = row.state.claim;
        if (
          claim.ownerId !== owner.ownerId
          || claim.ownerInstanceId !== owner.ownerInstanceId
          || claim.ownerGeneration !== owner.ownerGeneration
          || claim.authorityEpoch !== owner.authorityEpoch
        ) continue;
        if (
          claim.connectionGeneration === connectionGeneration
          && claim.phase === "fresh-fallback"
        ) continue;
        row.state = { kind: "ready" };
        released.push(row.identity.messageId);
      }
      return { changed: released.length > 0, value: released };
    });
  }

  listTerminal(
    accountKey: string,
  ): Promise<DurableOutcome<OutboundTerminalIntent[]>> {
    return this.transact(accountKey, (account) => ({
      changed: false,
      value: Object.values(account.terminals)
        .sort((left, right) => left.recordedAt - right.recordedAt)
        .map(cloneValue),
    }));
  }

  recordTerminal(
    identity: OutboundRowIdentity,
    kind: OutboundTerminalKind,
    expected: OutboundClaim,
  ): Promise<DurableOutcome<OutboundTerminalRecordResult>> {
    const intentId = crypto.randomUUID();
    return this.transact<OutboundTerminalRecordResult>(
      identity.accountKey,
      (account, authorityNow) => {
      const row = account.outbound[identity.messageId];
      if (!row || !sameIdentity(row.identity, identity)) {
        return { changed: false, value: { kind: "missing" } };
      }
      if (row.state.kind === "terminal") {
        const existing = account.terminals[row.state.intentId];
        return existing
          ? {
              changed: false,
              value: { kind: "recorded", intent: cloneValue(existing) },
            }
          : { changed: false, value: { kind: "stale" } };
      }
      if (!currentOwner(account, expected, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      if (row.state.kind !== "claimed" || !sameClaim(row.state.claim, expected)) {
        return { changed: false, value: { kind: "stale" } };
      }
      const intent: OutboundTerminalIntent = {
        intentId,
        accountKey: identity.accountKey,
        identity: { ...identity },
        kind,
        expected: { ...expected },
        recordedAt: authorityNow,
      };
      row.state = { kind: "terminal", intentId };
      account.terminals[intentId] = intent;
      return {
        changed: true,
        value: { kind: "recorded", intent: cloneValue(intent) },
      };
      },
    );
  }

  applyTerminal(
    executor: OutboundOwnerContext,
    intent: OutboundTerminalIntent,
  ): Promise<DurableOutcome<OutboundTerminalApplyResult>> {
    return this.transact<OutboundTerminalApplyResult>(
      intent.accountKey,
      (account, authorityNow) => {
      if (
        executor.accountKey !== intent.accountKey
        || !currentOwner(account, executor, authorityNow)
      ) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const persisted = account.terminals[intent.intentId];
      if (
        !claimMatchesIdentity(intent.expected, intent.identity)
        || !persisted
        || !sameIdentity(persisted.identity, intent.identity)
        || !sameClaim(persisted.expected, intent.expected)
        || persisted.kind !== intent.kind
        || !claimMatchesIdentity(persisted.expected, persisted.identity)
      ) {
        return { changed: false, value: { kind: "missing" } };
      }
      const row = account.outbound[intent.identity.messageId];
      if (
        !row
        || !sameIdentity(row.identity, intent.identity)
        || !claimMatchesIdentity(persisted.expected, row.identity)
      ) {
        delete account.terminals[intent.intentId];
        return { changed: true, value: { kind: "missing" } };
      }
      if (row.state.kind !== "terminal" || row.state.intentId !== intent.intentId) {
        delete account.terminals[intent.intentId];
        return { changed: true, value: { kind: "stale" } };
      }

      if (persisted.kind === "ack" || persisted.kind === "nonretryable-delete") {
        delete account.outbound[intent.identity.messageId];
        delete account.terminals[intent.intentId];
        return {
          changed: true,
          value: persisted.kind === "ack"
            ? { kind: "acked", identity: { ...row.identity } }
            : { kind: "removed", identity: { ...row.identity } },
        };
      }

      if (
        persisted.expected.phase === "resume-replay"
        && currentOwner(account, persisted.expected, authorityNow)
      ) {
        const claim: OutboundClaim = {
          ...persisted.expected,
          phase: "fresh-fallback",
          leaseUntil: checkedDurableDeadline(
            authorityNow,
            OUTBOUND_CLAIM_LEASE_MS,
            "Outbound claim lease",
          ),
        };
        row.state = { kind: "claimed", claim };
        delete account.terminals[intent.intentId];
        return {
          changed: true,
          value: { kind: "fallback", identity: { ...row.identity }, claim: { ...claim } },
        };
      }

      row.state = { kind: "ready" };
      delete account.terminals[intent.intentId];
      return {
        changed: true,
        value: { kind: "released", identity: { ...row.identity } },
      };
      },
    );
  }

  claimOwner(
    accountKey: string,
    hint: OutboundOwnerHint,
  ): Promise<DurableOutcome<OutboundOwnerActivation>> {
    const rotatedOwnerId = crypto.randomUUID();
    return this.transact(accountKey, (account, authorityNow) => {
      const existing = account.owners[hint.ownerId];
      let owner: DurableOutboundOwner;
      let handoffSm: DurableSmEnvelope | undefined;
      if (!existing) {
        owner = {
          ownerId: hint.ownerId,
          ownerInstanceId: hint.ownerInstanceId,
          ownerGeneration: allocateOwnerGeneration(account),
          authorityEpoch: account.authorityEpoch,
          leaseUntil: checkedDurableDeadline(
            authorityNow,
            OUTBOUND_CLAIM_LEASE_MS,
            "Outbound owner lease",
          ),
          lastRenewedAt: authorityNow,
        };
      } else if (
        existing.leaseUntil > authorityNow
        && existing.ownerInstanceId === hint.ownerInstanceId
        && existing.authorityEpoch === account.authorityEpoch
      ) {
        owner = {
          ...existing,
          leaseUntil: checkedDurableDeadline(
            authorityNow,
            OUTBOUND_CLAIM_LEASE_MS,
            "Outbound owner lease",
          ),
          lastRenewedAt: authorityNow,
        };
      } else if (
        existing.leaseUntil > authorityNow
        && existing.authorityEpoch === account.authorityEpoch
        && hint.handoffToken
        && existing.handoff?.token === hint.handoffToken
        && existing.handoff.expiresAt > authorityNow
        && existing.handoff.authorityEpoch === existing.authorityEpoch
        && existing.handoff.ownerGeneration === existing.ownerGeneration
      ) {
        const predecessors = retainedPredecessorsForHandoff(account, existing);
        owner = {
          ownerId: existing.ownerId,
          ownerInstanceId: hint.ownerInstanceId,
          ownerGeneration: allocateOwnerGeneration(account),
          authorityEpoch: account.authorityEpoch,
          leaseUntil: checkedDurableDeadline(
            authorityNow,
            OUTBOUND_CLAIM_LEASE_MS,
            "Outbound owner lease",
          ),
          lastRenewedAt: authorityNow,
          ...(predecessors.length > 0 ? { predecessors } : {}),
        };
        const smRecord = account.smSnapshots[existing.ownerId];
        const envelope = smRecord ? smEnvelope(smRecord) : null;
        if (
          smRecord
          && smRecord.ownerGeneration === existing.ownerGeneration
          && smRecord.authorityEpoch === existing.authorityEpoch
        ) {
          const handoffVersion = checkedDurableCounterIncrement(
            smRecord.version,
            "Durable SM version",
          );
          account.smSnapshots[existing.ownerId] = {
            ...smRecord,
            ownerGeneration: owner.ownerGeneration,
            authorityEpoch: owner.authorityEpoch,
            version: handoffVersion,
            consumed: true,
          };
          if (envelope && !smRecord.consumed) {
            handoffSm = {
              ...envelope,
              ownerGeneration: owner.ownerGeneration,
              authorityEpoch: owner.authorityEpoch,
              version: handoffVersion,
              consumed: true,
            };
          }
        }
      } else if (existing.ownerInstanceId === hint.ownerInstanceId) {
        owner = {
          // An expired or epoch-stale lifecycle is historical authority even
          // when the caller presents the same process incarnation. Keep its
          // owner row (and any claims/intents) immutable under the old key;
          // the reactivation receives a fresh owner key and generation.
          ownerId: rotatedOwnerId,
          ownerInstanceId: hint.ownerInstanceId,
          ownerGeneration: allocateOwnerGeneration(account),
          authorityEpoch: account.authorityEpoch,
          leaseUntil: checkedDurableDeadline(
            authorityNow,
            OUTBOUND_CLAIM_LEASE_MS,
            "Outbound owner lease",
          ),
          lastRenewedAt: authorityNow,
        };
      } else {
        owner = {
          ownerId: rotatedOwnerId,
          ownerInstanceId: hint.ownerInstanceId,
          ownerGeneration: allocateOwnerGeneration(account),
          authorityEpoch: account.authorityEpoch,
          leaseUntil: checkedDurableDeadline(
            authorityNow,
            OUTBOUND_CLAIM_LEASE_MS,
            "Outbound owner lease",
          ),
          lastRenewedAt: authorityNow,
        };
      }
      account.owners[owner.ownerId] = owner;
      return {
        changed: true,
        value: {
          fence: ownerFence(accountKey, owner),
          ...(handoffSm ? { handoffSm: cloneValue(handoffSm) } : {}),
        },
      };
    });
  }

  renewOwner(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<boolean>> {
    return this.transact<boolean>(owner.accountKey, (account, authorityNow) => {
      const persisted = account.owners[owner.ownerId];
      if (
        !currentOwner(account, owner, authorityNow)
      ) {
        return { changed: false, value: false };
      }
      persisted.leaseUntil = checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound owner lease",
      );
      persisted.lastRenewedAt = authorityNow;
      return { changed: true, value: true };
    });
  }

  preparePagehideHandoff(
    owner: OutboundOwnerContext,
    expectedSmVersion: number | null,
    handoffToken: string,
    state: PersistedSmResumeState | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<PagehideHandoffResult>>> {
    let snapshot: PersistedSmResumeState | null;
    try {
      snapshot = state
        ? decodePersistedSmResumeState(state, "pagehide.state")
        : null;
    } catch (error) {
      return Promise.resolve(failed(error));
    }
    return this.transact<DurableSmMutationResult<PagehideHandoffResult>>(
      owner.accountKey,
      (account, authorityNow) => {
      const persisted = account.owners[owner.ownerId];
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const existing = account.smSnapshots[owner.ownerId];
      const actualVersion = sameSmFence(existing, owner)
        ? existing.version
        : null;
      if (actualVersion !== expectedSmVersion) {
        return {
          changed: false,
          value: { kind: "stale", actualVersion },
        };
      }
      const version = checkedDurableCounterIncrement(
        existing?.version ?? 0,
        "Durable SM version",
      );
      account.smSnapshots[owner.ownerId] = {
        accountKey: owner.accountKey,
        ownerId: owner.ownerId,
        ownerGeneration: owner.ownerGeneration,
        authorityEpoch: owner.authorityEpoch,
        version,
        state: snapshot,
        savedAt: authorityNow,
        consumed: snapshot === null,
      };
      const handoffDeadline = checkedDurableDeadline(
        authorityNow,
        OUTBOUND_CLAIM_LEASE_MS,
        "Outbound handoff",
      );
      const handoff = {
        token: handoffToken,
        expiresAt: handoffDeadline,
        authorityEpoch: owner.authorityEpoch,
        ownerGeneration: owner.ownerGeneration,
      };
      persisted.handoff = handoff;
      persisted.leaseUntil = Math.max(persisted.leaseUntil, handoffDeadline);
      persisted.lastRenewedAt = authorityNow;
      return {
        changed: true,
        value: {
          kind: "applied",
          value: { handoff: { ...handoff }, smVersion: version },
        },
      };
      },
    );
  }

  cancelOwnerHandoff(
    owner: OutboundOwnerContext,
    expectedToken: string,
    expectedSmVersion: number,
  ): Promise<DurableOutcome<PagehideHandoffCancelResult>> {
    return this.transact<PagehideHandoffCancelResult>(
      owner.accountKey,
      (account, authorityNow) => {
      const persisted = account.owners[owner.ownerId];
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const smRecord = account.smSnapshots[owner.ownerId];
      if (
        persisted.handoff?.token !== expectedToken
        || persisted.handoff.authorityEpoch !== owner.authorityEpoch
        || persisted.handoff.ownerGeneration !== owner.ownerGeneration
        || !sameSmFence(smRecord, owner)
        || smRecord.version !== expectedSmVersion
      ) {
        return {
          changed: false,
          value: {
            kind: "stale",
            actualToken: persisted.handoff?.token ?? null,
            actualSmVersion: sameSmFence(smRecord, owner)
              ? smRecord.version
              : null,
          },
        };
      }
      delete persisted.handoff;
      return {
        changed: true,
        value: { kind: "applied", cancelled: true },
      };
      },
    );
  }

  loadSm(
    owner: OutboundOwnerContext,
  ): Promise<DurableOutcome<DurableSmLoadResult>> {
    return this.transact<DurableSmLoadResult>(
      owner.accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const envelope = account.smSnapshots[owner.ownerId];
      const ownedEnvelope = sameSmFence(envelope, owner)
        ? envelope
        : undefined;
      return {
        changed: false,
        value: {
          kind: "loaded",
          envelope: ownedEnvelope ? smEnvelope(ownedEnvelope) : null,
          version: ownedEnvelope?.version ?? null,
        },
      };
      },
    );
  }

  consumeSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    usable: (envelope: DurableSmEnvelope) => boolean,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope | null>>> {
    return this.transact<DurableSmMutationResult<DurableSmEnvelope | null>>(
      owner.accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const record = account.smSnapshots[owner.ownerId];
      const ownedRecord = sameSmFence(record, owner) ? record : undefined;
      const actualVersion = ownedRecord?.version ?? null;
      if (actualVersion !== expectedVersion) {
        return {
          changed: false,
          value: { kind: "stale", actualVersion },
        };
      }
      const envelope = ownedRecord ? smEnvelope(ownedRecord) : null;
      if (
        !ownedRecord
        || !envelope
        || ownedRecord.consumed
        || !usable(cloneValue(envelope))
      ) {
        return {
          changed: false,
          value: { kind: "applied", value: null },
        };
      }
      const consumed: DurableSmEnvelope = {
        ...envelope,
        version: checkedDurableCounterIncrement(
          ownedRecord.version,
          "Durable SM version",
        ),
        consumed: true,
      };
      account.smSnapshots[owner.ownerId] = {
        ...ownedRecord,
        version: consumed.version,
        consumed: true,
      };
      return {
        changed: true,
        value: { kind: "applied", value: cloneValue(consumed) },
      };
      },
    );
  }

  saveSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
    state: PersistedSmResumeState,
    savedAt: number,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmEnvelope>>> {
    let snapshot: PersistedSmResumeState;
    try {
      snapshot = decodePersistedSmResumeState(state, "save.state");
    } catch (error) {
      return Promise.resolve(failed(error));
    }
    return this.transact<DurableSmMutationResult<DurableSmEnvelope>>(
      owner.accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const existing = account.smSnapshots[owner.ownerId];
      const ownedExisting = sameSmFence(existing, owner) ? existing : undefined;
      const actualVersion = ownedExisting?.version ?? null;
      if (actualVersion !== expectedVersion) {
        return {
          changed: false,
          value: { kind: "stale", actualVersion },
        };
      }
      const envelope: DurableSmEnvelope = {
        accountKey: owner.accountKey,
        ownerId: owner.ownerId,
        ownerGeneration: owner.ownerGeneration,
        authorityEpoch: owner.authorityEpoch,
        version: checkedDurableCounterIncrement(
          existing?.version ?? 0,
          "Durable SM version",
        ),
        state: snapshot,
        savedAt: authorityNow,
        consumed: false,
      };
      account.smSnapshots[owner.ownerId] = {
        ...envelope,
        state: cloneSmResumeState(envelope.state),
      };
      return {
        changed: true,
        value: { kind: "applied", value: cloneValue(envelope) },
      };
      },
    );
  }

  clearSm(
    owner: OutboundOwnerContext,
    expectedVersion: number | null,
  ): Promise<DurableOutcome<DurableSmMutationResult<DurableSmClearResult>>> {
    return this.transact<DurableSmMutationResult<DurableSmClearResult>>(
      owner.accountKey,
      (account, authorityNow) => {
      if (!currentOwner(account, owner, authorityNow)) {
        return { changed: false, value: { kind: "fenced" } };
      }
      const existing = account.smSnapshots[owner.ownerId];
      const ownedExisting = sameSmFence(existing, owner) ? existing : undefined;
      const actualVersion = ownedExisting?.version ?? null;
      if (actualVersion !== expectedVersion) {
        return {
          changed: false,
          value: { kind: "stale", actualVersion },
        };
      }
      const version = checkedDurableCounterIncrement(
        existing?.version ?? 0,
        "Durable SM version",
      );
      account.smSnapshots[owner.ownerId] = {
        accountKey: owner.accountKey,
        ownerId: owner.ownerId,
        ownerGeneration: owner.ownerGeneration,
        authorityEpoch: owner.authorityEpoch,
        version,
        state: null,
        savedAt: authorityNow,
        consumed: true,
      };
      return {
        changed: true,
        value: {
          kind: "applied",
          value: { cleared: !!ownedExisting?.state, version },
        },
      };
      },
    );
  }
}

export class MemoryDurableOutboundStore extends RuntimeDurableStore {
  private readonly accounts = new Map<string, RuntimeAccount>();
  private transactionTail: Promise<void> = Promise.resolve();

  constructor(
    authorityClock: DurableAuthorityClock = systemAuthorityClock,
    private readonly beforeTransaction?: () => Promise<void>,
  ) {
    super(authorityClock);
  }

  protected transact<T>(
    accountKey: string,
    mutate: (account: RuntimeAccount, authorityNow: number) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>> {
    const operation = this.transactionTail.then(async (): Promise<DurableOutcome<T>> => {
      await this.beforeTransaction?.();
      const existing = this.accounts.get(accountKey);
      const account = existing
        ? decodeRuntimeAccount(cloneValue(existing), accountKey)
        : emptyAccount(accountKey);
      const previousAuthorityTime = account.lastAuthorityTimeMs;
      const {
        authorityNow,
        metadataChanged,
        authorityEpochChanged,
      } = this.sampleAuthorityTime(account);
      const mutation = mutate(account, authorityNow);
      account.lastAuthorityTimeMs = authorityNow;
      if (mutation.changed || authorityEpochChanged) {
        account.revision = checkedDurableCounterIncrement(
          account.revision,
          "Durable account revision",
        );
      }
      const committedValue = mutation.finalize
        ? mutation.finalize(account.revision)
        : mutation.value;
      if (
        mutation.changed
        || metadataChanged
        || authorityNow !== previousAuthorityTime
      ) {
        this.accounts.set(accountKey, account);
      }
      return { kind: "committed", value: cloneValue(committedValue) };
    }).catch((error) => failed<T>(error));
    this.transactionTail = operation.then(() => undefined);
    return operation;
  }
}

export type IndexedDbDurableOutboundStoreOptions = {
  authorityClock?: DurableAuthorityClock;
  databaseName?: string;
  databaseVersion?: number;
  indexedDb?: IDBFactory;
};

function openDatabase(
  indexedDb: IDBFactory | undefined,
  databaseName: string,
  databaseVersion: number,
): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    if (!indexedDb) {
      reject(new DOMException("IndexedDB is unavailable", "NotSupportedError"));
      return;
    }
    let settled = false;
    const rejectOnce = (error: unknown): void => {
      if (settled) return;
      settled = true;
      reject(error);
    };
    const request = indexedDb.open(databaseName, databaseVersion);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(ACCOUNT_STORE_NAME)) {
        request.result.createObjectStore(ACCOUNT_STORE_NAME, { keyPath: "accountKey" });
      }
    };
    request.onsuccess = () => {
      if (settled) {
        request.result.close();
        return;
      }
      settled = true;
      resolve(request.result);
    };
    request.onerror = () => rejectOnce(
      request.error ?? new Error("IndexedDB open failed"),
    );
    request.onblocked = () => rejectOnce(
      new DOMException("IndexedDB upgrade blocked", "AbortError"),
    );
  });
}

export class IndexedDbDurableOutboundStore extends RuntimeDurableStore {
  private readonly indexedDb: IDBFactory | undefined;
  private readonly databaseName: string;
  private readonly databaseVersion: number;
  private databasePromise: Promise<IDBDatabase> | null = null;

  constructor(options: IndexedDbDurableOutboundStoreOptions = {}) {
    super(options.authorityClock ?? systemAuthorityClock);
    this.indexedDb = options.indexedDb ?? globalThis.indexedDB;
    this.databaseName = options.databaseName ?? DATABASE_NAME;
    this.databaseVersion = options.databaseVersion ?? DATABASE_VERSION;
    if (
      !Number.isSafeInteger(this.databaseVersion)
      || this.databaseVersion < 1
    ) {
      throw new RangeError("IndexedDB databaseVersion must be a positive integer");
    }
  }

  private database(): Promise<IDBDatabase> {
    if (this.databasePromise) return this.databasePromise;
    const opening = openDatabase(
      this.indexedDb,
      this.databaseName,
      this.databaseVersion,
    );
    this.databasePromise = opening;
    void opening.then(
      (database) => {
        database.onversionchange = () => {
          database.close();
          if (this.databasePromise === opening) this.databasePromise = null;
        };
      },
      () => {
        if (this.databasePromise === opening) this.databasePromise = null;
      },
    );
    return opening;
  }

  async close(): Promise<void> {
    const opened = this.databasePromise;
    this.databasePromise = null;
    if (!opened) return;
    try {
      (await opened).close();
    } catch {
      // A failed/blocked open has no connection to close.
    }
  }

  protected async transact<T>(
    accountKey: string,
    mutate: (account: RuntimeAccount, authorityNow: number) => AccountMutation<T>,
  ): Promise<DurableOutcome<T>> {
    let database: IDBDatabase;
    try {
      database = await this.database();
    } catch (error) {
      return failed(error);
    }

    return new Promise((resolve) => {
      let value: T | undefined;
      let valueReady = false;
      let operationError: unknown;
      let transaction: IDBTransaction;
      try {
        transaction = database.transaction(
          ACCOUNT_STORE_NAME,
          "readwrite",
          { durability: "strict" },
        );
        const store = transaction.objectStore(ACCOUNT_STORE_NAME);
        const read = store.get(accountKey);
        read.onsuccess = () => {
          try {
            const account = read.result === undefined
              ? emptyAccount(accountKey)
              : decodeRuntimeAccount(read.result, accountKey);
            const previousAuthorityTime = account.lastAuthorityTimeMs;
            const {
              authorityNow,
              metadataChanged,
              authorityEpochChanged,
            } = this.sampleAuthorityTime(account);
            const mutation = mutate(account, authorityNow);
            account.lastAuthorityTimeMs = authorityNow;
            if (mutation.changed || authorityEpochChanged) {
              account.revision = checkedDurableCounterIncrement(
                account.revision,
                "Durable account revision",
              );
            }
            value = mutation.finalize
              ? mutation.finalize(account.revision)
              : mutation.value;
            valueReady = true;
            if (
              !mutation.changed
              && !metadataChanged
              && authorityNow === previousAuthorityTime
            ) return;
            const write = store.put(account);
            write.onerror = () => {
              operationError = write.error ?? new Error("IndexedDB account write failed");
              transaction.abort();
            };
          } catch (error) {
            operationError = error;
            transaction.abort();
          }
        };
        read.onerror = () => {
          operationError = read.error ?? new Error("IndexedDB account read failed");
          transaction.abort();
        };
      } catch (error) {
        resolve(failed(error));
        return;
      }
      transaction.oncomplete = () => {
        if (!valueReady) {
          resolve(failed(new DOMException("IndexedDB operation did not settle", "AbortError")));
          return;
        }
        resolve({ kind: "committed", value: cloneValue(value as T) });
      };
      transaction.onerror = () => {
        // `onabort` owns the single terminal result.
      };
      transaction.onabort = () => resolve(failed(operationError ?? transaction.error));
    });
  }
}
