/** XEP-0394: Message Markup — build and parse <markup> XML elements. */

export interface MarkupSpan {
  type: "b" | "i" | "s" | "code" | "code-block" | "blockquote" | "link";
  start: number;
  end: number;
  uri?: string;
}

export const NS_MARKUP = "urn:xmpp:markup:0";

const VALID_TYPES = new Set<MarkupSpan["type"]>([
  "b", "i", "s", "code", "code-block", "blockquote", "link",
]);

function escapeXmlAttr(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Build an XML string for the `<markup>` element from an array of spans.
 * Returns null if spans array is empty.
 */
export function buildMarkupElement(spans: MarkupSpan[]): string | null {
  if (spans.length === 0) return null;

  const sorted = [...spans].sort((a, b) => a.start - b.start);
  const children = sorted.map((span) => {
    const uri = span.type === "link" && span.uri
      ? ` uri="${escapeXmlAttr(span.uri)}"`
      : "";
    return `<${span.type} start="${span.start}" end="${span.end}"${uri}/>`;
  });

  return `<markup xmlns="${NS_MARKUP}">${children.join("")}</markup>`;
}

/**
 * Parse a `<markup>` element from a received message into MarkupSpan[].
 * Accepts raw XML string or a stanza.js parsed element (Record with children).
 * Returns empty array if no markup found.
 */
export function parseMarkupElement(xml: string | Record<string, unknown>): MarkupSpan[] {
  if (typeof xml === "string") return parseFromString(xml);
  return parseFromObject(xml);
}

// Matches self-closing child elements inside <markup>
const CHILD_RE = /<([\w-]+)\s+([^/>]*)\/?>/g;
const ATTR_RE = /([\w-]+)="([^"]*)"/g;

function parseFromString(xml: string): MarkupSpan[] {
  const spans: MarkupSpan[] = [];

  let childMatch: RegExpExecArray | null;
  while ((childMatch = CHILD_RE.exec(xml)) !== null) {
    const tag = childMatch[1] as string;
    const attrStr = childMatch[2] as string;
    if (!VALID_TYPES.has(tag as MarkupSpan["type"])) continue;

    const attrs: Record<string, string> = {};
    let attrMatch: RegExpExecArray | null;
    ATTR_RE.lastIndex = 0;
    while ((attrMatch = ATTR_RE.exec(attrStr)) !== null) {
      attrs[attrMatch[1] as string] = attrMatch[2] as string;
    }

    const start = Number(attrs.start);
    const end = Number(attrs.end);
    if (Number.isNaN(start) || Number.isNaN(end)) continue;

    const span: MarkupSpan = { type: tag as MarkupSpan["type"], start, end };
    if (tag === "link" && attrs.uri) span.uri = attrs.uri;
    spans.push(span);
  }

  return spans;
}

function parseFromObject(el: Record<string, unknown>): MarkupSpan[] {
  const spans: MarkupSpan[] = [];
  const children = (el.children ?? el.xml?.toString?.()) as unknown;

  // stanza.js elements expose a `children` array of sub-elements
  if (Array.isArray(children)) {
    for (const child of children) {
      if (typeof child !== "object" || child === null) continue;
      const c = child as Record<string, unknown>;
      const tag = (c.name ?? c.localName ?? c.tag) as string | undefined;
      if (!tag || !VALID_TYPES.has(tag as MarkupSpan["type"])) continue;

      const attrs = (c.attrs ?? c.attributes ?? c) as Record<string, unknown>;
      const start = Number(attrs.start);
      const end = Number(attrs.end);
      if (Number.isNaN(start) || Number.isNaN(end)) continue;

      const span: MarkupSpan = { type: tag as MarkupSpan["type"], start, end };
      if (tag === "link" && attrs.uri) span.uri = String(attrs.uri);
      spans.push(span);
    }
    return spans;
  }

  // Fallback: if the object has an xml string representation, parse that
  const xmlStr = el.xml ?? el.toString?.();
  if (typeof xmlStr === "string" && xmlStr.includes("<")) {
    return parseFromString(xmlStr);
  }

  return spans;
}
