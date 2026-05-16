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
 */
export function findMessageIndexById<T extends MessageIdCarrier>(
  messages: readonly T[],
  candidate: string | null | undefined,
): number {
  const normalized = normalizeMessageId(candidate);
  if (!normalized) return -1;

  const primaryIndex = messages.findIndex((message) => message.id === normalized);
  if (primaryIndex >= 0) return primaryIndex;

  let aliasIndex = -1;
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i]!;
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
): T | undefined {
  const index = findMessageIndexById(messages, candidate);
  return index < 0 ? undefined : messages[index];
}

/**
 * Index `message` under its primary id and each wire alias. When an alias
 * is already indexed against a different message (sender-controlled
 * `origin-id` reuse with distinct content per waddle-social/waddle#484),
 * the alias is dropped from the index entirely so neither message can be
 * resolved through it. Primary ids are never dropped — they uniquely
 * identify their owning message.
 */
export function indexMessageByIds<T extends MessageIdCarrier>(
  index: Map<string, T>,
  message: T,
): void {
  index.set(message.id, message);
  for (const alias of message.wireIds ?? []) {
    if (alias === message.id) continue;
    const existing = index.get(alias);
    if (!existing) {
      index.set(alias, message);
      continue;
    }
    if (existing.id === message.id) continue;
    // Collision: two distinct messages claim the same wire alias. Drop the
    // alias entirely so no caller can resolve it to either candidate.
    index.delete(alias);
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
