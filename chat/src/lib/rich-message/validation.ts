import type { MarkupSpan, MessageReference } from "./types";

export function normalizeOutboundMarkup(markup: MarkupSpan[]): MarkupSpan[] {
  return markup
    .filter(hasValidMarkupRange)
    .map((span) =>
      span.type === "span"
        ? { ...span, styles: Array.from(new Set(span.styles)).sort() }
        : span
    )
    .sort((a, b) => a.start - b.start || a.end - b.end || a.type.localeCompare(b.type));
}

export function normalizeOutboundReferences(references: MessageReference[]): MessageReference[] {
  return references
    .filter((reference) => {
      if (!reference.uri) return false;
      if (typeof reference.begin !== "number" || typeof reference.end !== "number") return false;
      return reference.begin >= 0 && reference.end > reference.begin;
    })
    .sort((a, b) => (a.begin ?? 0) - (b.begin ?? 0) || (a.end ?? 0) - (b.end ?? 0));
}

export function hasValidMarkupRange(span: MarkupSpan): boolean {
  if (!Number.isFinite(span.start) || !Number.isFinite(span.end)) return false;
  if (span.start < 0 || span.end <= span.start) return false;
  if (span.type === "span") return span.styles.length > 0;
  if (span.type === "list") {
    return span.items.length > 0
      && span.items[0] === span.start
      && span.items.every((item) => item >= span.start && item < span.end);
  }
  return true;
}
