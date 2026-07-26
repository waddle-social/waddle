/**
 * The privacy-safe call correlation id (#1452) — the join key that lets a
 * Faro call event, a server call-setup log line, and a LiveKit webhook log
 * line be shown as one call.
 *
 * It deliberately introduces **no new identifier**: the LiveKit room name
 * already flows client → server → SFU → webhook (the client receives it in
 * the issued `urn:waddle:transports:livekit:0` transport), so no XMPP wire
 * element is invented for it.
 *
 * The raw room name cannot be the attribute: for 1:1 calls it is
 * `<initiator-bare-jid>::<sid>` and for Muji calls it is the MUC room JID,
 * both of which carry identity. So the shared key is a truncated SHA-256
 * digest of the room name — bounded, stable across the three vantage
 * points, and not reversible without already knowing the room.
 *
 * The Rust side is `server/crates/waddle-sfu/src/correlation.rs`. The two
 * implementations must stay byte-for-byte compatible: lowercase hex of the
 * first {@link CALL_CORRELATION_ID_HEX_LEN} / 2 digest bytes.
 */

/** Hex characters kept from the digest. Must match the Rust constant. */
export const CALL_CORRELATION_ID_HEX_LEN = 16;

/** The attribute value used when no call is in scope. */
export const UNKNOWN_CALL_CORRELATION_ID = "unknown";

/**
 * The LiveKit room name of a 1:1 call, derived the way the server
 * derives it (`scoped_call_id` in
 * `server/crates/waddle-xmpp/src/protocol/handlers/jingle.rs`:
 * `{initiator_bare}::{sid}`). This lets lifecycle events for calls that
 * never connected — declined and failed attempts have no join token —
 * still carry the same correlation id the server and webhook logs use.
 * The format is cross-pinned by digest test vectors on both sides, so a
 * server-side rename breaks CI instead of silently un-joining telemetry.
 * The result feeds ONLY the SHA-256 correlation digest; the raw name
 * carries the initiator JID and is never exported.
 */
export function dmCallRoomName(initiatorBareJid: string, sid: string): string {
  return `${initiatorBareJid}::${sid}`;
}

function toHexPrefix(digest: ArrayBuffer): string {
  return Array.from(new Uint8Array(digest).slice(0, CALL_CORRELATION_ID_HEX_LEN / 2))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Derive the correlation id for a LiveKit room name.
 *
 * Resolves to {@link UNKNOWN_CALL_CORRELATION_ID} when the room name is
 * empty or `crypto.subtle` is unavailable (a non-secure context). Telemetry
 * must never be the reason a call fails, so this never rejects.
 */
export async function deriveCallCorrelationId(roomName: string): Promise<string> {
  if (!roomName) return UNKNOWN_CALL_CORRELATION_ID;
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return UNKNOWN_CALL_CORRELATION_ID;
  try {
    const digest = await subtle.digest("SHA-256", new TextEncoder().encode(roomName));
    return toHexPrefix(digest);
  } catch {
    return UNKNOWN_CALL_CORRELATION_ID;
  }
}

/**
 * The correlation id of the call currently in scope. Module-scoped rather
 * than threaded through every telemetry call site: exactly one call is
 * active at a time (the call store enforces a single call slot), and every
 * call-scoped Faro event wants the same value.
 */
let currentCallCorrelationId: string = UNKNOWN_CALL_CORRELATION_ID;

/** Bumped on every adopt/clear so a digest that settles after the call
 *  ended (or after a newer call adopted) cannot re-arm a stale id. */
let correlationEpoch = 0;

/** Read the correlation id to stamp onto call-scoped telemetry. */
export function callCorrelationId(): string {
  return currentCallCorrelationId;
}

/**
 * Adopt `roomName`'s correlation id as the current one. Called when the
 * engine connects to a LiveKit room. Awaitable so tests are deterministic;
 * production callers may fire-and-forget.
 */
export async function adoptCallCorrelationId(roomName: string): Promise<string> {
  correlationEpoch += 1;
  const epoch = correlationEpoch;
  const id = await deriveCallCorrelationId(roomName);
  if (epoch === correlationEpoch) currentCallCorrelationId = id;
  return id;
}

/** Drop the correlation id when the call ends, so a later event that
 *  escapes the call scope cannot be attributed to the previous call. */
export function clearCallCorrelationId(): void {
  correlationEpoch += 1;
  currentCallCorrelationId = UNKNOWN_CALL_CORRELATION_ID;
}
