/** XEP-0394: Message Markup — stanza.js extension. */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";
import type { MarkupSpan } from "@/lib/chat-ui";

const NS_MARKUP_0 = "urn:xmpp:markup:0";

export type WaddleMarkupSpan = MarkupSpan;

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
    const shifted = {
      ...span,
      start: span.start + offset,
      end: span.end + offset,
      ...("items" in span && Array.isArray(span.items)
        ? { items: span.items.map((item) => item + offset) }
        : {}),
    };
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
      ...("items" in span && Array.isArray(span.items)
        ? { items: span.items.map((item) => rebaseOffsetAfterRemoval(item, start, end)) }
        : {}),
    };
    return hasValidOffsets(rebased) ? [rebased] : [];
  });
}

const STYLE_NAMES = new Set(["strong", "emphasis", "deleted", "code"]);

function parseOffset(value: string | undefined | null): number | null {
  if (!value) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function childName(el: XMLElement): string {
  return el.getName?.() ?? (el as unknown as Record<string, string>).name;
}

/** Custom field: reads/writes child markup elements inside <markup>. */
const spansField: FieldDefinition<WaddleMarkupSpan[]> = {
  importer(xml: XMLElement): WaddleMarkupSpan[] | undefined {
    const spans: WaddleMarkupSpan[] = [];
    for (const child of xml.children) {
      if (typeof child === "string") continue;
      const el = child as XMLElement;
      const tag = childName(el);
      const start = parseOffset(el.getAttribute("start"));
      const end = parseOffset(el.getAttribute("end"));
      if (!tag || start === null || end === null) continue;

      if (tag === "span") {
        const styles = el.children.flatMap((styleChild) => {
          if (typeof styleChild === "string") return [];
          const name = childName(styleChild as XMLElement);
          return STYLE_NAMES.has(name) ? [name as "strong" | "emphasis" | "deleted" | "code"] : [];
        });
        if (styles.length > 0) spans.push({ type: "span", start, end, styles });
        continue;
      }

      if (tag === "bcode") {
        const language = el.getAttribute("language");
        spans.push({ type: "bcode", start, end, ...(language ? { language } : {}) });
        continue;
      }

      if (tag === "bquote") {
        spans.push({ type: "bquote", start, end });
        continue;
      }

      if (tag === "list") {
        const ordered = el.getAttribute("ordered") === "true";
        const items = el.children.flatMap((itemChild) => {
          if (typeof itemChild === "string") return [];
          const itemEl = itemChild as XMLElement;
          if (childName(itemEl) !== "li") return [];
          const itemStart = parseOffset(itemEl.getAttribute("start"));
          return itemStart === null ? [] : [itemStart];
        });
        if (items.length > 0) spans.push({ type: "list", start, end, ordered, items });
      }
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
      if (span.type === "span") {
        const child = new XMLElement("span", attrs);
        for (const style of span.styles) {
          child.appendChild(new XMLElement(style));
        }
        xml.appendChild(child);
      } else if (span.type === "bcode") {
        if (span.language) attrs.language = span.language;
        xml.appendChild(new XMLElement("bcode", attrs));
      } else if (span.type === "bquote") {
        xml.appendChild(new XMLElement("bquote", attrs));
      } else if (span.type === "list") {
        const child = new XMLElement("list", {
          ...attrs,
          ordered: span.ordered ? "true" : "false",
        });
        for (const itemStart of span.items) {
          child.appendChild(new XMLElement("li", { start: String(itemStart) }));
        }
        xml.appendChild(child);
      }
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
