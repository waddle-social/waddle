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
