/** XEP-0394: Message Markup — stanza.js extension. */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";

const NS_MARKUP_0 = "urn:xmpp:markup:0";

export interface WaddleMarkupSpan {
  type: string;
  start: number;
  end: number;
  uri?: string;
}

type MarkupSpanWithOffsets = {
  start: number;
  end: number;
};

function hasValidOffsets(span: MarkupSpanWithOffsets): boolean {
  return Number.isFinite(span.start) && Number.isFinite(span.end) && span.start >= 0 && span.end > span.start;
}

export function shiftMarkupSpans<T extends MarkupSpanWithOffsets>(spans: readonly T[], offset: number): T[] {
  if (!Number.isFinite(offset)) return [];
  return spans.flatMap((span) => {
    if (!hasValidOffsets(span)) return [];
    const shifted = { ...span, start: span.start + offset, end: span.end + offset };
    return hasValidOffsets(shifted) ? [shifted] : [];
  });
}

function rebaseOffsetAfterRemoval(offset: number, start: number, end: number): number {
  if (offset <= start) return offset;
  if (offset >= end) return offset - (end - start);
  return start;
}

export function stripMarkupRange<T extends MarkupSpanWithOffsets>(spans: readonly T[], start: number, end: number): T[] {
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) return [];
  return spans.flatMap((span) => {
    if (!hasValidOffsets(span)) return [];
    const rebased = {
      ...span,
      start: rebaseOffsetAfterRemoval(span.start, start, end),
      end: rebaseOffsetAfterRemoval(span.end, start, end),
    };
    return hasValidOffsets(rebased) ? [rebased] : [];
  });
}

const VALID_TYPES = new Set(["b", "i", "s", "code", "code-block", "blockquote", "link"]);

/** Custom field: reads/writes child span elements inside <markup>. */
const spansField: FieldDefinition<WaddleMarkupSpan[]> = {
  importer(xml: XMLElement): WaddleMarkupSpan[] | undefined {
    const spans: WaddleMarkupSpan[] = [];
    for (const child of xml.children) {
      if (typeof child === "string") continue;
      const el = child as XMLElement;
      const tag = el.getName?.() ?? (el as unknown as Record<string, string>).name;
      if (!tag || !VALID_TYPES.has(tag)) continue;
      const start = Number(el.getAttribute("start"));
      const end = Number(el.getAttribute("end"));
      if (Number.isNaN(start) || Number.isNaN(end)) continue;
      const span: WaddleMarkupSpan = { type: tag, start, end };
      if (tag === "link") {
        const uri = el.getAttribute("uri");
        if (uri) span.uri = uri;
      }
      spans.push(span);
    }
    return spans.length > 0 ? spans : undefined;
  },
  exporter(xml: XMLElement, value: WaddleMarkupSpan[]): void {
    if (!value || value.length === 0) return;
    for (const span of value) {
      const attrs: Record<string, string> = {
        start: String(span.start),
        end: String(span.end),
      };
      if (span.type === "link" && span.uri) {
        attrs.uri = span.uri;
      }
      const child = new XMLElement(span.type, attrs);
      xml.appendChild(child);
    }
  },
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.markup", multiple: false }],
    element: "markup",
    fields: {
      spans: spansField,
    },
    namespace: NS_MARKUP_0,
  },
];

export default definitions;
