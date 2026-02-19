export interface EventPayloadEnvelope<TData = Record<string, unknown>> {
  type?: string;
  data?: TData;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function asEventPayloadEnvelope<TData = Record<string, unknown>>(
  value: unknown,
): EventPayloadEnvelope<TData> | null {
  if (!isObject(value)) return null;

  const type = typeof value.type === 'string' ? value.type : undefined;
  const data = isObject(value.data) ? (value.data as TData) : undefined;
  if (!type && !data) return null;

  return { type, data };
}

/**
 * Normalize transport payloads across browser and Tauri event emitters.
 * Handles both:
 * - `{ channel, payload: { type, data } }`
 * - `{ type, data }`
 */
export function normalizeEventPayload<TData = Record<string, unknown>>(
  input: unknown,
): EventPayloadEnvelope<TData> | null {
  if (!isObject(input)) return null;
  return asEventPayloadEnvelope<TData>(input.payload) ?? asEventPayloadEnvelope<TData>(input);
}

export function extractMessageFromEventPayload<TMessage>(input: unknown): TMessage | null {
  const payload = normalizeEventPayload<{ message?: TMessage }>(input);
  return payload?.data?.message ?? null;
}
