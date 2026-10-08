import { bareJidKey } from "@/lib/xmpp/jid";

interface MessageIdCarrier {
  id: string;
  wireIds?: string[];
  rowKey?: string;
  authorOccupantJid?: string;
  stanzaId?: string;
  stanzaIdBy?: string;
}

function roomCanonicalId(message: MessageIdCarrier): string | undefined {
  return message.stanzaId && message.stanzaIdBy && message.authorOccupantJid
    && bareJidKey(message.stanzaIdBy) === bareJidKey(message.authorOccupantJid)
    ? message.stanzaId : undefined;
}

function normalizeMessageId(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

function dedupeMessageIds(ids: readonly (string | null | undefined)[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];

  for (const candidate of ids) {
    const normalized = normalizeMessageId(candidate);
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    out.push(normalized);
  }

  return out;
}

function splitMessageIds(
  primaryId: string | null | undefined,
  extraIds: readonly (string | null | undefined)[] = [],
): MessageIdCarrier {
  const ids = dedupeMessageIds([primaryId, ...extraIds]);
  const id = ids[0] ?? crypto.randomUUID();
  const wireIds = ids.slice(1);
  return wireIds.length > 0 ? { id, wireIds } : { id };
}

function findRoomMessageIndexById<T extends MessageIdCarrier>(
  messages: readonly T[],
  normalized: string,
  predicate?: (message: T) => boolean,
): number {
  const candidates = messages.map((message, index) => ({ message, index }))
    .filter(({ message }) => !predicate || predicate(message));
  const canonical = candidates.filter(({ message }) => roomCanonicalId(message) === normalized);
  if (canonical.length > 0) return canonical.length === 1 ? canonical[0]!.index : -1;
  const claimants = candidates.filter(({ message }) =>
    message.id === normalized || message.wireIds?.includes(normalized));
  return claimants.length === 1 ? claimants[0]!.index : -1;
}

function findDmMessageIndexById<T extends MessageIdCarrier>(
  messages: readonly T[],
  normalized: string,
  predicate?: (message: T) => boolean,
): number {
  let aliasIndex = -1;
  let aliasAmbiguous = false;
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i]!;
    if (predicate && !predicate(message)) continue;
    // Primary ids win over alias ambiguity, so the scan must complete
    // before the ambiguity verdict: bailing out on the second alias
    // claimant would hide a later row whose PRIMARY id matches (e.g. a
    // redelivered copy reconciling into its own row while two other rows
    // share the candidate as a reused origin-id alias) — the caller would
    // then append a duplicate instead of merging.
    if (message.id === normalized) return i;
    if (!message.wireIds?.includes(normalized)) continue;
    if (aliasIndex < 0) {
      aliasIndex = i;
      continue;
    }
    if (messages[aliasIndex]!.id !== message.id) aliasAmbiguous = true;
  }
  return aliasAmbiguous ? -1 : aliasIndex;
}

/**
 * Room references prefer verified canonical stanza IDs and reject duplicate
 * authored claims. For DMs, primary `id` wins; a `wireIds` alias
 * only resolves when exactly one message in the input claims it. When two
 * distinct messages share the same alias (XEP-0359 origin-id reuse with
 * fresh stanza-ids per waddle-social/waddle#484) the lookup returns -1
 * rather than silently picking one — destructive callers (retractions,
 * corrections, displayed markers) must not target either candidate in
 * that case.
 *
 * An optional `predicate` narrows the candidate set without losing
 * collision-safety semantics: filtered-out messages neither match nor
 * count toward alias ambiguity, so e.g. delivery-event handlers can
 * scope the lookup to `m.isSelf` without a non-self primary id swallowing
 * a self message's wire-alias.
 */
export function findMessageIndexById<T extends MessageIdCarrier>(
  messages: readonly T[],
  candidate: string | null | undefined,
  predicate?: (message: T) => boolean,
): number {
  const normalized = normalizeMessageId(candidate);
  if (!normalized) return -1;
  if (messages.some((message) => !!message.authorOccupantJid)) {
    return findRoomMessageIndexById(messages, normalized, predicate);
  }
  return findDmMessageIndexById(messages, normalized, predicate);
}

export function findMessageById<T extends MessageIdCarrier>(
  messages: readonly T[],
  candidate: string | null | undefined,
  predicate?: (message: T) => boolean,
): T | undefined {
  const index = findMessageIndexById(messages, candidate, predicate);
  return index < 0 ? undefined : messages[index];
}

/**
 * Collision-safe id index. Maintains separate primary-id and alias-id
 * lookups plus a tombstone set so a later collision can never:
 *
 *   - drop the primary-id mapping of an earlier message (which would
 *     break canonical lookups), or
 *   - re-introduce an ambiguous alias mapping after the collision has
 *     been observed.
 *
 * Room indexes retain distinct claimants and prioritize verified room stanza
 * IDs. DM primary IDs retain their existing replacement semantics. Aliases (`wireIds`) only
 * resolve when no other message has claimed the same value either as a
 * primary id or as an alias.
 */
export class MessageIdIndex<T extends MessageIdCarrier> {
  private readonly roomClaims = new Map<string, Map<string | T, T>>();
  private readonly roomCanonical = new Map<string, Map<string | T, T>>();
  private readonly primary = new Map<string, T>();
  private readonly aliases = new Map<string, T>();
  private readonly ambiguousAliases = new Set<string>();

  add(message: T): void {
    if (message.authorOccupantJid) {
      this.addRoom(message);
      return;
    }
    this.addDm(message);
  }

  private roomClaimIdentity(message: T, canonical: string | undefined): string | T {
    if (message.rowKey) return `row:${message.rowKey}`;
    if (canonical) return `canonical:${bareJidKey(message.stanzaIdBy!)}\u0000${canonical}`;
    return message;
  }

  private addRoom(message: T): void {
    const canonical = roomCanonicalId(message);
    const identity = this.roomClaimIdentity(message, canonical);
    for (const id of new Set([message.id, ...(message.wireIds ?? [])])) {
      const claims = this.roomClaims.get(id) ?? new Map<string | T, T>();
      claims.set(identity, message);
      this.roomClaims.set(id, claims);
    }
    if (canonical) {
      const claims = this.roomCanonical.get(canonical) ?? new Map<string | T, T>();
      claims.set(identity, message);
      this.roomCanonical.set(canonical, claims);
    }
  }

  private addDm(message: T): void {
    this.primary.set(message.id, message);
    for (const alias of message.wireIds ?? []) {
      if (alias === message.id) continue;
      if (this.ambiguousAliases.has(alias)) {
        this.aliases.delete(alias);
        continue;
      }
      const existingPrimary = this.primary.get(alias);
      if (existingPrimary && existingPrimary.id !== message.id) {
        // Alias collides with another message's canonical primary id.
        // Keep the primary lookup intact; tombstone the alias so future
        // lookups via `alias` return only the primary owner and no
        // ambiguous alias claimer.
        this.ambiguousAliases.add(alias);
        this.aliases.delete(alias);
        continue;
      }
      const existingAlias = this.aliases.get(alias);
      if (existingAlias && existingAlias.id !== message.id) {
        // Two distinct messages claim the same alias — tombstone it so
        // a third claimant can't sneak back into the unambiguous slot.
        this.ambiguousAliases.add(alias);
        this.aliases.delete(alias);
        continue;
      }
      this.aliases.set(alias, message);
    }
  }

  get(candidate: string | null | undefined): T | undefined {
    const normalized = normalizeMessageId(candidate);
    if (!normalized) return undefined;
    const canonical = this.roomCanonical.get(normalized);
    if (canonical) return canonical.size === 1 ? canonical.values().next().value : undefined;
    const room = this.roomClaims.get(normalized);
    if (room) {
      if (this.primary.has(normalized) || this.aliases.has(normalized) || this.ambiguousAliases.has(normalized)) return undefined;
      return room.size === 1 ? room.values().next().value : undefined;
    }
    const primary = this.primary.get(normalized);
    if (primary) return primary;
    if (this.ambiguousAliases.has(normalized)) return undefined;
    return this.aliases.get(normalized);
  }

  has(candidate: string | null | undefined): boolean {
    return this.get(candidate) !== undefined;
  }
}

export function mergeMessageIds<T extends MessageIdCarrier>(
  message: T,
  primaryId: string | null | undefined,
  extraIds: readonly (string | null | undefined)[] = [],
): T {
  const normalized = splitMessageIds(
    primaryId,
    [message.id, ...(message.wireIds ?? []), ...extraIds],
  );

  if (normalized.wireIds?.length) {
    return { ...message, id: normalized.id, wireIds: normalized.wireIds };
  }

  const { wireIds: _wireIds, ...rest } = message;
  return { ...rest, id: normalized.id } as T;
}
