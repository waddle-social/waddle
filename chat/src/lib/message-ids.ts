interface MessageIdCarrier {
  id: string;
  wireIds?: string[];
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

/**
 * Collision-safe alias lookup. Primary `id` always wins; a `wireIds` alias
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

  let aliasIndex = -1;
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i]!;
    if (predicate && !predicate(message)) continue;
    if (message.id === normalized) return i;
    if (!message.wireIds?.includes(normalized)) continue;
    if (aliasIndex < 0) {
      aliasIndex = i;
      continue;
    }
    if (messages[aliasIndex]!.id !== message.id) return -1;
  }
  return aliasIndex;
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
 * Primary ids are unique by construction (UUIDs / server stanza-ids) so
 * they always resolve to their owning message. Aliases (`wireIds`) only
 * resolve when no other message has claimed the same value either as a
 * primary id or as an alias.
 */
export class MessageIdIndex<T extends MessageIdCarrier> {
  private readonly primary = new Map<string, T>();
  private readonly aliases = new Map<string, T>();
  private readonly ambiguousAliases = new Set<string>();

  add(message: T): void {
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
