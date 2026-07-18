import type { PersistedQueuedMessage } from "../outbound-queue-store";
import { decodePersistedSmResumeState } from "../xmpp/sm-resume-types";
import {
  RETAINED_PREDECESSOR_LIMIT,
  outboundLane,
  type DurableOutboundEntryState,
  type OutboundClaim,
  type OutboundLane,
  type OutboundOwnerContext,
  type OutboundOwnerFence,
  type OutboundOwnerHandoff,
  type OutboundRowIdentity,
  type OutboundTerminalIntent,
} from "./durable-contract";
import {
  claimMatchesIdentity,
  dictionary,
  orderKey,
  sameIdentity,
  sameLane,
  sameOwner,
  type DurableOutboundOwner,
  type DurableOutboundRow,
  type DurablePredecessorFence,
  type DurableSmRecord,
  type RuntimeAccount,
} from "./durable-model";

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

export function decodeRuntimeAccount(
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

export async function outboundPayloadDigest(
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
