import type { TimelineMessage } from "@/lib/chat-ui";
import { barePeerJid } from "@/lib/xmpp-client";

function normalizedRealJid(message: TimelineMessage): string | undefined {
  return message.authorRealJid ? barePeerJid(message.authorRealJid) : undefined;
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
  const primaryMatches = messages.filter(
    (message) =>
      message.id === incoming.id
      && !hasConflictingRoomCanonicalIdentity(message, incoming)
      && hasMessageSenderContinuity(message, incoming),
  );
  if (primaryMatches.length === 1) return primaryMatches[0];
  if (primaryMatches.length > 1) return undefined;

  let target: TimelineMessage | undefined;
  for (const id of [incoming.id, ...(incoming.wireIds ?? [])]) {
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
