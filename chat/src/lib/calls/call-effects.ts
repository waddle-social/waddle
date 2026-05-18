import { outboundCalls, type CallWireSender } from "./outbound";
import type { CallEvent, CallState } from "./types";

/**
 * Side-effect handler invoked by the chat-side `on_call` wrapper
 * AFTER the reducer has updated `$callState`. Keeps the reducer
 * itself pure: the only thing this module does is translate
 * inbound events into outbound wire stanzas where XEP-0353 /
 * XEP-0166 requires the originator to respond.
 *
 * Current responsibility: on inbound `<proceed/>` while we have an
 * outgoing call with a matching `sid`, fire the Jingle
 * `session-initiate` IQ using the responder's full JID stamped on
 * `proceed.from` (XEP-0353 §0.6 requires the JMI responder to
 * surface their full JID so the initiator can address the next
 * Jingle stanza directly).
 *
 * `prev` is the snapshot of the store BEFORE `reduceCallState`
 * applied the event — that's important because the reducer keeps
 * `outgoing` across `proceed`, so reading `prev` and the latest
 * state would both work, but reading `prev` makes the dependency
 * explicit and avoids subtle store-replay bugs if the reducer
 * grows new transitions later.
 */
export async function handleCallEventSideEffect(
  event: CallEvent,
  prev: CallState,
  sender: CallWireSender,
  ourFullJid: string,
): Promise<void> {
  if (event.kind !== "proceed") return;
  if (prev.phase !== "outgoing") return;
  if (prev.sid !== event.sid) return;
  // `event.from` is the responder's full JID (XEP-0353 §0.6).
  await outboundCalls.sessionInitiate(
    sender,
    event.from,
    ourFullJid,
    event.sid,
    prev.media,
  );
}
