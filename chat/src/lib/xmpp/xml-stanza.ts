// Sending the IQ requests this client builds as raw XML (see
// `xml-build.ts`), with the disco timeout and cancellation.

import { withIqTimeout } from "./discovery";

export {
  buildIqXml,
  childElements,
  firstElementNS,
  parseXmlResponse,
  submitField,
  type XmlElementSpec,
} from "./xml-build";

/** The raw IQ surface of the WASM client this module needs. */
export type RawIqClient = {
  send_raw_iq?: (xml: string) => Promise<string>;
  cancel_raw_iq?: (id: string) => Promise<unknown>;
};

/**
 * Send a serialized IQ and resolve with the response XML. Rejects with the
 * stanza error the client reports, or with a timeout error when no answer
 * arrives within `timeoutMs` (the disco default when omitted).
 */
export function sendRawIq(
  xmpp: RawIqClient,
  request: { xml: string; id: string; to: string; node?: string; timeoutMs?: number },
): Promise<string> {
  const send = xmpp.send_raw_iq;
  if (!send) return Promise.reject(new Error("XMPP session is not ready"));
  const cancel = xmpp.cancel_raw_iq;
  return withIqTimeout(
    send.call(xmpp, request.xml),
    request.to,
    request.node,
    request.timeoutMs,
    cancel ? { cancel: async () => { await cancel.call(xmpp, request.id); } } : {},
  );
}
