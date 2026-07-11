import type { TimelineMessage } from "@/lib/chat-ui";
import { bareJidKey, barePeerJid } from "@/lib/xmpp-client";

function normalizedRealJid(message: TimelineMessage): string | undefined {
  return message.authorRealJid
    ? barePeerJid(message.authorRealJid).trim().toLowerCase()
    : undefined;
}

function senderScopeJid(message: TimelineMessage): string | undefined {
  return message.authorOccupantJid ?? message.authorJid;
}

function roomCanonicalIdentity(message: TimelineMessage): string | undefined {
  if (
    !message.authorOccupantJid
    || !message.stanzaId
    || !message.stanzaIdBy
  ) return undefined;
  return `${barePeerJid(message.stanzaIdBy).toLowerCase()}\u0000${message.stanzaId}`;
}

function hasConflictingRoomCanonicalIdentity(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): boolean {
  const existingIdentity = roomCanonicalIdentity(existing);
  const incomingIdentity = roomCanonicalIdentity(incoming);
  return !!existingIdentity && !!incomingIdentity && existingIdentity !== incomingIdentity;
}

/**
 * Establishes continuity without treating a display nick as identity.
 * Real JIDs take precedence when both copies expose them; otherwise the
 * full occupant JID bridges an optimistic/live copy that lacks the real JID.
 */
export function hasMessageSenderContinuity(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): boolean {
  if (
    existing.authorOccupantJid
    && incoming.authorOccupantJid
    && existing.authorOccupantJid !== incoming.authorOccupantJid
  ) return false;

  const existingRealJid = normalizedRealJid(existing);
  const incomingRealJid = normalizedRealJid(incoming);
  if (existingRealJid && incomingRealJid) return existingRealJid === incomingRealJid;

  const existingOccupantJid = senderScopeJid(existing);
  const incomingOccupantJid = senderScopeJid(incoming);
  if (existing.authorOccupantJid || incoming.authorOccupantJid) {
    return !!existingOccupantJid
      && !!incomingOccupantJid
      && existingOccupantJid === incomingOccupantJid;
  }

  const existingAuthorJid = bareJidKey(existing.authorJid ?? "");
  const incomingAuthorJid = bareJidKey(incoming.authorJid ?? "");
  return !!existingAuthorJid
    && !!incomingAuthorJid
    && existing.author === incoming.author
    && existingAuthorJid === incomingAuthorJid;
}

/**
 * Resolves sender-chosen ids only inside a verified sender scope. A primary
 * match wins; aliases must identify exactly one row, and aliases that point
 * at different rows fail closed.
 */
export function findSenderScopedIdTarget(
  messages: readonly TimelineMessage[],
  incoming: TimelineMessage,
): TimelineMessage | undefined {
  const primaryMatches = messages.filter(
    (message) =>
      message.id === incoming.id
      && !hasConflictingRoomCanonicalIdentity(message, incoming)
      && hasMessageSenderContinuity(message, incoming),
  );
  if (primaryMatches.length === 1) return primaryMatches[0];
  if (primaryMatches.length > 1) return undefined;

  let target: TimelineMessage | undefined;
  for (const id of new Set([incoming.id, ...(incoming.wireIds ?? [])])) {
    const matches = messages.filter(
      (message) =>
        (message.id === id || message.wireIds?.includes(id))
        && !hasConflictingRoomCanonicalIdentity(message, incoming)
        && hasMessageSenderContinuity(message, incoming),
    );
    if (matches.length > 1) return undefined;
    const match = matches[0];
    if (!match) continue;
    if (target && target !== match) return undefined;
    target = match;
  }
  return target;
}

type Resolution = {
  ambiguous: boolean;
  match?: TimelineMessage;
};

function addToIndex(
  index: Map<string, Set<TimelineMessage>>,
  key: string | undefined,
  message: TimelineMessage,
): void {
  if (!key) return;
  const matches = index.get(key) ?? new Set<TimelineMessage>();
  matches.add(message);
  index.set(key, matches);
}

function removeFromIndex(
  index: Map<string, Set<TimelineMessage>>,
  key: string | undefined,
  message: TimelineMessage,
): void {
  if (!key) return;
  const matches = index.get(key);
  matches?.delete(message);
  if (matches?.size === 0) index.delete(key);
}

function combineResolutions(resolutions: readonly Resolution[]): Resolution {
  let match: TimelineMessage | undefined;
  for (const resolution of resolutions) {
    if (resolution.ambiguous) return { ambiguous: true };
    if (!resolution.match) continue;
    if (match && match !== resolution.match) return { ambiguous: true };
    match = resolution.match;
  }
  return match ? { ambiguous: false, match } : { ambiguous: false };
}

function resolveSets(sets: readonly (Set<TimelineMessage> | undefined)[]): Resolution {
  let match: TimelineMessage | undefined;
  for (const messages of sets) {
    if (!messages || messages.size === 0) continue;
    if (messages.size > 1) return { ambiguous: true };
    const candidate = messages.values().next().value;
    if (!candidate) continue;
    if (match && match !== candidate) return { ambiguous: true };
    match = candidate;
  }
  return match ? { ambiguous: false, match } : { ambiguous: false };
}

function continuityKey(...parts: readonly string[]): string {
  return parts.join("\u0000");
}

function legacySenderKey(message: TimelineMessage): string | undefined {
  const authorJid = bareJidKey(message.authorJid ?? "");
  return authorJid ? continuityKey(message.author, authorJid) : undefined;
}

class SenderContinuityIndex {
  private messageCount = 0;
  private readonly occupantAll = new Map<string, Set<TimelineMessage>>();
  private readonly occupantByReal = new Map<string, Set<TimelineMessage>>();
  private readonly occupantByJidAndReal = new Map<string, Set<TimelineMessage>>();
  private readonly occupantWithoutReal = new Map<string, Set<TimelineMessage>>();
  private readonly accountAllByLegacy = new Map<string, Set<TimelineMessage>>();
  private readonly accountAllByScope = new Map<string, Set<TimelineMessage>>();
  private readonly accountByReal = new Map<string, Set<TimelineMessage>>();
  private readonly accountWithoutRealByLegacy = new Map<string, Set<TimelineMessage>>();
  private readonly accountWithoutRealByScope = new Map<string, Set<TimelineMessage>>();

  constructor(private readonly noteProbe: () => void) {}

  get empty(): boolean {
    return this.messageCount === 0;
  }

  private get(
    index: Map<string, Set<TimelineMessage>>,
    key: string | undefined,
  ): Set<TimelineMessage> | undefined {
    this.noteProbe();
    return key ? index.get(key) : undefined;
  }

  add(message: TimelineMessage): void {
    this.messageCount += 1;
    const occupant = message.authorOccupantJid;
    const real = normalizedRealJid(message) || undefined;
    if (occupant) {
      addToIndex(this.occupantAll, occupant, message);
      if (real) {
        addToIndex(this.occupantByReal, real, message);
        addToIndex(this.occupantByJidAndReal, continuityKey(occupant, real), message);
      } else {
        addToIndex(this.occupantWithoutReal, occupant, message);
      }
      return;
    }

    const scope = senderScopeJid(message) || undefined;
    const legacy = legacySenderKey(message);
    addToIndex(this.accountAllByLegacy, legacy, message);
    addToIndex(this.accountAllByScope, scope, message);
    if (real) {
      addToIndex(this.accountByReal, real, message);
    } else {
      addToIndex(this.accountWithoutRealByLegacy, legacy, message);
      addToIndex(this.accountWithoutRealByScope, scope, message);
    }
  }

  remove(message: TimelineMessage): void {
    this.messageCount -= 1;
    const occupant = message.authorOccupantJid;
    const real = normalizedRealJid(message) || undefined;
    if (occupant) {
      removeFromIndex(this.occupantAll, occupant, message);
      if (real) {
        removeFromIndex(this.occupantByReal, real, message);
        removeFromIndex(this.occupantByJidAndReal, continuityKey(occupant, real), message);
      } else {
        removeFromIndex(this.occupantWithoutReal, occupant, message);
      }
      return;
    }

    const scope = senderScopeJid(message) || undefined;
    const legacy = legacySenderKey(message);
    removeFromIndex(this.accountAllByLegacy, legacy, message);
    removeFromIndex(this.accountAllByScope, scope, message);
    if (real) {
      removeFromIndex(this.accountByReal, real, message);
    } else {
      removeFromIndex(this.accountWithoutRealByLegacy, legacy, message);
      removeFromIndex(this.accountWithoutRealByScope, scope, message);
    }
  }

  resolve(incoming: TimelineMessage): Resolution {
    const occupant = incoming.authorOccupantJid;
    const real = normalizedRealJid(incoming) || undefined;
    if (occupant && real) {
      return resolveSets([
        this.get(this.occupantByJidAndReal, continuityKey(occupant, real)),
        this.get(this.occupantWithoutReal, occupant),
        this.get(this.accountByReal, real),
        this.get(this.accountWithoutRealByScope, occupant),
      ]);
    }
    if (occupant) {
      return resolveSets([
        this.get(this.occupantAll, occupant),
        this.get(this.accountAllByScope, occupant),
      ]);
    }

    const scope = senderScopeJid(incoming) || undefined;
    const legacy = legacySenderKey(incoming);
    if (real) {
      return resolveSets([
        this.get(this.accountByReal, real),
        this.get(this.accountWithoutRealByLegacy, legacy),
        this.get(this.occupantByReal, real),
        this.get(this.occupantWithoutReal, scope),
      ]);
    }
    return resolveSets([
      this.get(this.accountAllByLegacy, legacy),
      this.get(this.occupantAll, scope),
    ]);
  }
}

class SenderIdBucket {
  private readonly messages = new Map<TimelineMessage, number>();
  private readonly all: SenderContinuityIndex;
  private readonly withoutCanonical: SenderContinuityIndex;
  private readonly byCanonical = new Map<string, SenderContinuityIndex>();

  constructor(private readonly noteProbe: () => void) {
    this.all = new SenderContinuityIndex(noteProbe);
    this.withoutCanonical = new SenderContinuityIndex(noteProbe);
  }

  get empty(): boolean {
    return this.messages.size === 0;
  }

  get canonicalPartitionCount(): number {
    return this.byCanonical.size;
  }

  add(message: TimelineMessage): void {
    const occurrences = this.messages.get(message) ?? 0;
    this.messages.set(message, occurrences + 1);
    if (occurrences > 0) return;
    this.all.add(message);
    const canonical = roomCanonicalIdentity(message);
    if (!canonical) {
      this.withoutCanonical.add(message);
      return;
    }
    const index = this.byCanonical.get(canonical) ?? new SenderContinuityIndex(this.noteProbe);
    index.add(message);
    this.byCanonical.set(canonical, index);
  }

  remove(message: TimelineMessage): void {
    const occurrences = this.messages.get(message) ?? 0;
    if (occurrences === 0) return;
    if (occurrences > 1) {
      this.messages.set(message, occurrences - 1);
      return;
    }
    this.messages.delete(message);
    this.all.remove(message);
    const canonical = roomCanonicalIdentity(message);
    if (!canonical) {
      this.withoutCanonical.remove(message);
      return;
    }
    const index = this.byCanonical.get(canonical);
    index?.remove(message);
    if (index?.empty) this.byCanonical.delete(canonical);
  }

  resolve(incoming: TimelineMessage): Resolution {
    const canonical = roomCanonicalIdentity(incoming);
    const resolution = !canonical
      ? this.all.resolve(incoming)
      : combineResolutions([
      this.byCanonical.get(canonical)?.resolve(incoming) ?? { ambiguous: false },
      this.withoutCanonical.resolve(incoming),
    ]);
    if (resolution.match && (this.messages.get(resolution.match) ?? 0) > 1) {
      return { ambiguous: true };
    }
    return resolution;
  }
}

/** Collision-preserving identity index for repeated MAM reconciliation. */
export class SenderScopedIdIndex {
  private readonly byId = new Map<string, SenderIdBucket>();
  private readonly primaryById = new Map<string, SenderIdBucket>();
  private probes = 0;

  constructor(messages: readonly TimelineMessage[] = []) {
    for (const message of messages) this.add(message);
  }

  /** Stable work counter for complexity regression tests and diagnostics. */
  get resolutionProbeCount(): number {
    return this.probes;
  }

  /** Number of live canonical sub-indexes retained across every ID bucket. */
  get retainedCanonicalPartitionCount(): number {
    return [...this.byId.values(), ...this.primaryById.values()]
      .reduce((total, bucket) => total + bucket.canonicalPartitionCount, 0);
  }

  private noteProbe = (): void => {
    this.probes += 1;
  };

  private addToBucket(
    buckets: Map<string, SenderIdBucket>,
    id: string,
    message: TimelineMessage,
  ): void {
    const bucket = buckets.get(id) ?? new SenderIdBucket(this.noteProbe);
    bucket.add(message);
    buckets.set(id, bucket);
  }

  private removeFromBucket(
    buckets: Map<string, SenderIdBucket>,
    id: string,
    message: TimelineMessage,
  ): void {
    const bucket = buckets.get(id);
    bucket?.remove(message);
    if (bucket?.empty) buckets.delete(id);
  }

  add(message: TimelineMessage): void {
    for (const id of new Set([message.id, ...(message.wireIds ?? [])])) {
      this.addToBucket(this.byId, id, message);
    }
    this.addToBucket(this.primaryById, message.id, message);
  }

  replace(existing: TimelineMessage, replacement: TimelineMessage): void {
    for (const id of new Set([existing.id, ...(existing.wireIds ?? [])])) {
      this.removeFromBucket(this.byId, id, existing);
    }
    this.removeFromBucket(this.primaryById, existing.id, existing);
    this.add(replacement);
  }

  find(incoming: TimelineMessage): TimelineMessage | undefined {
    const primary = this.primaryById.get(incoming.id)?.resolve(incoming);
    if (primary?.ambiguous) return undefined;
    if (primary?.match) return primary.match;

    let target: TimelineMessage | undefined;
    for (const id of new Set([incoming.id, ...(incoming.wireIds ?? [])])) {
      const resolution = this.byId.get(id)?.resolve(incoming);
      if (resolution?.ambiguous) return undefined;
      const match = resolution?.match;
      if (!match) continue;
      if (target && target !== match) return undefined;
      target = match;
    }
    return target;
  }
}
