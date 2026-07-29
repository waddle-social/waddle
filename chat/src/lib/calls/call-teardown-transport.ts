/**
 * Transport policy for best-effort call-teardown stanza sends (#1446),
 * split out of `call-store` so the deadline/retry machinery lives
 * apart from call state and lifecycle bookkeeping.
 *
 * Teardown wire sends are peer/mixer notifications — they must never
 * hold the local call slot (or a caller like `hangupActiveCall`)
 * hostage to a server that withholds its reply.
 */
const DEFAULT_TEARDOWN_STANZA_TIMEOUT_MS = 10_000;

/**
 * XEP-0166 session-terminate is the one teardown stanza worth a second
 * attempt: it is what ends the call for the peer, and re-sending a sid
 * the peer already forgot is harmless. Presence/retract/reject sends
 * stay single-attempt — a duplicate delivered late is not.
 */
export const TERMINATE_SEND_ATTEMPTS = 2;

/**
 * Explicit per-send policy — passed down from the caller (tests supply
 * a short deadline directly) instead of living as mutable module
 * state.
 */
export type TeardownSendPolicy = {
  /** Deadline per attempt. Defaults to {@link DEFAULT_TEARDOWN_STANZA_TIMEOUT_MS}. */
  stanzaTimeoutMs?: number;
  /** Total tries; only a timeout triggers another attempt. Defaults to 1. */
  attempts?: number;
  /**
   * Invoked after every expired attempt (before any retry) — the hook
   * where callers cancel the underlying XMPP operation, so a stale
   * command cannot linger in the WASM driver's pending/deferred queues
   * after the JS side has moved on.
   */
  onTimeout?: () => void;
};

class TeardownSendTimeoutError extends Error {
  constructor() {
    super("call teardown stanza timed out");
  }
}

/**
 * Race a teardown wire send against the deadline, retrying on timeout
 * up to `attempts` total tries. A typed error reply from the server is
 * a definitive answer and is NOT retried — only silence is. The
 * abandoned attempt's promise is left to settle in the void; its
 * result no longer matters (`onTimeout` is where the underlying
 * operation gets cancelled).
 */
export async function boundedTeardownSend<T>(
  send: () => Promise<T>,
  policy: TeardownSendPolicy = {},
): Promise<T> {
  const timeoutMs = policy.stanzaTimeoutMs ?? DEFAULT_TEARDOWN_STANZA_TIMEOUT_MS;
  const attempts = policy.attempts ?? 1;
  for (let attempt = 1; ; attempt += 1) {
    let timer: ReturnType<typeof setTimeout> | null = null;
    try {
      return await Promise.race([
        send(),
        new Promise<never>((_resolve, reject) => {
          timer = setTimeout(() => reject(new TeardownSendTimeoutError()), timeoutMs);
        }),
      ]);
    } catch (err) {
      if (err instanceof TeardownSendTimeoutError) policy.onTimeout?.();
      if (!(err instanceof TeardownSendTimeoutError) || attempt >= attempts) throw err;
    } finally {
      if (timer) clearTimeout(timer);
    }
  }
}
