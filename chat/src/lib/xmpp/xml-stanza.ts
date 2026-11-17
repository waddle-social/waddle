// Structured construction of the IQ requests this client sends as raw XML.
//
// Elements are built with the DOM and serialized once, so attribute values
// and text are escaped by the serializer rather than by string assembly. An
// element read from an earlier response can be placed in a new request
// unchanged (the DOM adopts it), which is how unknown extensions are kept
// when an item is republished.

import { withIqTimeout } from "./discovery";

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

export interface XmlElementSpec {
  name: string;
  /** Namespace of the element; omitted to inherit the parent's. */
  ns?: string;
  /** Attributes; an `undefined` value is left out. */
  attrs?: Record<string, string | undefined>;
  text?: string;
  children?: ReadonlyArray<XmlElementSpec | Element>;
}

/** Serialized `<iq/>` request carrying one payload element. */
export function buildIqXml(request: {
  type: "get" | "set";
  id: string;
  to?: string;
  payload: XmlElementSpec;
}): string {
  if (typeof document === "undefined" || typeof XMLSerializer === "undefined") {
    throw new Error("XML serialization is not available");
  }
  const doc = document.implementation.createDocument("", "iq");
  const iq = doc.documentElement;
  iq.setAttribute("type", request.type);
  iq.setAttribute("id", request.id);
  if (request.to) iq.setAttribute("to", request.to);
  iq.appendChild(buildElement(doc, request.payload, null));
  return new XMLSerializer().serializeToString(iq);
}

function buildElement(doc: XMLDocument, spec: XmlElementSpec, parentNs: string | null): Element {
  const ns = spec.ns ?? parentNs;
  const element = doc.createElementNS(ns, spec.name);
  for (const [name, value] of Object.entries(spec.attrs ?? {})) {
    if (value !== undefined) element.setAttribute(name, value);
  }
  if (spec.text !== undefined) element.appendChild(doc.createTextNode(spec.text));
  for (const child of spec.children ?? []) {
    element.appendChild(isElementSpec(child) ? buildElement(doc, child, ns) : child);
  }
  return element;
}

function isElementSpec(value: XmlElementSpec | Element): value is XmlElementSpec {
  return typeof (value as Element).localName !== "string";
}

/** Data-form `submit` field with one value (XEP-0004). */
export function submitField(name: string, value: string, type?: string): XmlElementSpec {
  return {
    name: "field",
    attrs: { var: name, type },
    children: [{ name: "value", text: value }],
  };
}

/** Parse a response document. */
export function parseXmlResponse(xml: string): Document {
  return new DOMParser().parseFromString(xml, "text/xml");
}

/** Direct children of `parent` with this local name and namespace. */
export function childElements(parent: Element, localName: string, namespace: string): Element[] {
  return Array.from(parent.children).filter(
    (child) => child.localName === localName && child.namespaceURI === namespace,
  );
}

/** First element in `doc` with this local name and namespace. */
export function firstElementNS(doc: Document | Element, namespace: string, localName: string): Element | null {
  return doc.getElementsByTagNameNS(namespace, localName)[0] ?? null;
}
