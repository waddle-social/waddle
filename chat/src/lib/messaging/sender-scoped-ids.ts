import type { TimelineMessage } from "@/lib/chat-ui";
import { barePeerJid } from "@/lib/xmpp-client";

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

  return existing.author === incoming.author
    && barePeerJid(existing.authorJid ?? "") === barePeerJid(incoming.authorJid ?? "");
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
  return new SenderScopedIdIndex(messages).find(incoming);
}

/** Collision-preserving identity index for repeated MAM reconciliation. */
export class SenderScopedIdIndex {
  private readonly byId = new Map<string, Set<TimelineMessage>>();

  constructor(messages: readonly TimelineMessage[] = []) {
    for (const message of messages) this.add(message);
  }

  add(message: TimelineMessage): void {
    for (const id of new Set([message.id, ...(message.wireIds ?? [])])) {
      const bucket = this.byId.get(id) ?? new Set<TimelineMessage>();
      bucket.add(message);
      this.byId.set(id, bucket);
    }
  }

  replace(existing: TimelineMessage, replacement: TimelineMessage): void {
    for (const id of new Set([existing.id, ...(existing.wireIds ?? [])])) {
      const bucket = this.byId.get(id);
      bucket?.delete(existing);
      if (bucket?.size === 0) this.byId.delete(id);
    }
    this.add(replacement);
  }

  private matches(id: string, incoming: TimelineMessage): TimelineMessage[] {
    return [...(this.byId.get(id) ?? [])].filter(
      (message) =>
        !hasConflictingRoomCanonicalIdentity(message, incoming)
        && hasMessageSenderContinuity(message, incoming),
    );
  }

  find(incoming: TimelineMessage): TimelineMessage | undefined {
    const primaryMatches = this.matches(incoming.id, incoming).filter(
      (message) => message.id === incoming.id,
    );
    if (primaryMatches.length === 1) return primaryMatches[0];
    if (primaryMatches.length > 1) return undefined;

    let target: TimelineMessage | undefined;
    for (const id of new Set([incoming.id, ...(incoming.wireIds ?? [])])) {
      const matches = this.matches(id, incoming);
      if (matches.length > 1) return undefined;
      const match = matches[0];
      if (!match) continue;
      if (target && target !== match) return undefined;
      target = match;
    }
    return target;
  }
}
