/**
 * Extraction of RFC 6120 §8.3 stanza-error context from a rejected wasm
 * promise, shared by every flow that reports XMPP failures to telemetry
 * (muc#admin member list, disco, MUC join). The bridge rejects with an
 * `Error` carrying `condition` / `errorType` / `text` properties (or the
 * legacy `{ condition }` / `{ error: { condition } }` shapes); anything
 * else yields an empty context.
 */
import type { XmppStanzaErrorType } from "@/lib/xmpp/types";

export interface StanzaErrorContext {
  condition?: string;
  errorType?: XmppStanzaErrorType;
  errorText?: string;
}

/** Cap on server-provided error text forwarded to telemetry — RFC 6120
 * puts no size limit on `<text/>`, and beacon context should stay small. */
const MAX_ERROR_TEXT_LENGTH = 256;

const STANZA_ERROR_TYPES: ReadonlySet<string> = new Set([
  "auth",
  "cancel",
  "continue",
  "modify",
  "wait",
  "unknown",
]);

export function stanzaErrorContext(error: unknown): StanzaErrorContext {
  const source = stanzaErrorSource(error);
  if (!source) return {};
  const { condition, errorType, text } = source as {
    condition?: unknown;
    errorType?: unknown;
    text?: unknown;
  };
  return {
    ...(typeof condition === "string" ? { condition } : {}),
    ...(isStanzaErrorType(errorType) ? { errorType } : {}),
    ...(typeof text === "string" && text.length > 0
      ? { errorText: text.slice(0, MAX_ERROR_TEXT_LENGTH) }
      : {}),
  };
}

function isStanzaErrorType(value: unknown): value is XmppStanzaErrorType {
  return typeof value === "string" && STANZA_ERROR_TYPES.has(value);
}

function stanzaErrorSource(error: unknown): object | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  if (typeof (error as { condition?: unknown }).condition === "string") return error;
  const nested = (error as { error?: unknown }).error;
  if (typeof nested !== "object" || nested === null) return undefined;
  return typeof (nested as { condition?: unknown }).condition === "string" ? nested : undefined;
}
