export interface MessageIdCarrier {
  id: string;
  wireIds?: string[];
}

function normalizeMessageId(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

export function dedupeMessageIds(ids: readonly (string | null | undefined)[]): string[] {
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

export function splitMessageIds(
  primaryId: string | null | undefined,
  extraIds: readonly (string | null | undefined)[] = [],
): MessageIdCarrier {
  const ids = dedupeMessageIds([primaryId, ...extraIds]);
  const id = ids[0] ?? crypto.randomUUID();
  const wireIds = ids.slice(1);
  return wireIds.length > 0 ? { id, wireIds } : { id };
}

export function matchMessageId(
  message: MessageIdCarrier,
  candidate: string | null | undefined,
): boolean {
  const normalized = normalizeMessageId(candidate);
  return !!normalized && (
    message.id === normalized
    || message.wireIds?.includes(normalized) === true
  );
}

export function findMessageById<T extends MessageIdCarrier>(
  messages: readonly T[],
  candidate: string | null | undefined,
): T | undefined {
  return messages.find((message) => matchMessageId(message, candidate));
}

export function indexMessageByIds<T extends MessageIdCarrier>(
  index: Map<string, T>,
  message: T,
): void {
  for (const id of [message.id, ...(message.wireIds ?? [])]) {
    index.set(id, message);
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
