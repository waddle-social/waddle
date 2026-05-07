import type { XmppSendIq } from "./types";

export function requireRawIq(xmpp: XmppSendIq): (xml: string) => Promise<string> {
  if (typeof xmpp.send_raw_iq !== "function") {
    throw new Error("XMPP raw IQ sender is unavailable.");
  }
  return xmpp.send_raw_iq.bind(xmpp);
}
