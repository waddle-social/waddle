import { codePointLength, sliceByCodePoints } from "@/lib/text-offsets";
import {
  codeRangesFromMarkup,
  inferBareUrlReferences,
  safeUri,
} from "./urls";
import { hasValidMarkupRange } from "./validation";
import type {
  BlockNode,
  InlineNode,
  MarkupSpan,
  MessageReference,
  TiptapMark,
} from "./types";
import { BLOCK_TYPES, MARK_BY_STYLE } from "./types";

interface ParseContext {
  body: string;
  length: number;
  markup: MarkupSpan[];
  spans: Extract<MarkupSpan, { type: "span" }>[];
  blocks: Exclude<MarkupSpan, { type: "span" }>[];
  references: MessageReference[];
}

export function richMessageToTiptap(input: {
  body: string;
  markup?: readonly MarkupSpan[];
  references?: readonly MessageReference[];
}): BlockNode {
  const context = createParseContext(input.body, input.markup ?? [], input.references ?? []);
  const content = blocksFromRange(context, 0, context.length);
  return {
    type: "doc",
    content: content.length > 0 ? content : [{ type: "paragraph" }],
  };
}

function createParseContext(
  body: string,
  markup: readonly MarkupSpan[],
  references: readonly MessageReference[],
): ParseContext {
  const length = codePointLength(body);
  const validMarkup = markup
    .filter((span): span is MarkupSpan => isValidInboundMarkup(span, length))
    .sort((a, b) => a.start - b.start || b.end - a.end);
  const validReferences = references
    .filter((reference) => isValidInboundReference(reference, length))
    .sort((a, b) => (a.begin ?? 0) - (b.begin ?? 0) || (a.end ?? 0) - (b.end ?? 0));
  const inferredReferences = inferBareUrlReferences(
    body,
    validReferences,
    codeRangesFromMarkup(validMarkup),
  );

  return {
    body,
    length,
    markup: validMarkup,
    spans: validMarkup.filter((span): span is Extract<MarkupSpan, { type: "span" }> => span.type === "span"),
    blocks: validMarkup.filter((span): span is Exclude<MarkupSpan, { type: "span" }> => BLOCK_TYPES.has(span.type)),
    references: [...validReferences, ...inferredReferences]
      .sort((a, b) => (a.begin ?? 0) - (b.begin ?? 0) || (a.end ?? 0) - (b.end ?? 0)),
  };
}

function isValidInboundMarkup(span: MarkupSpan, length: number): boolean {
  if (!hasValidMarkupRange(span)) return false;
  if (span.end > length) return false;
  if (span.type === "span") {
    return span.styles.every((style) => style in MARK_BY_STYLE);
  }
  return true;
}

function isValidInboundReference(reference: MessageReference, length: number): boolean {
  if (!reference.uri) return false;
  if (reference.type !== "data") return false;
  if (typeof reference.begin !== "number" || typeof reference.end !== "number") return false;
  if (reference.begin < 0 || reference.end <= reference.begin || reference.end > length) return false;
  return !!safeUri(reference.uri);
}

function blocksFromRange(context: ParseContext, start: number, end: number): BlockNode[] {
  const blocks: BlockNode[] = [];
  const topLevelBlocks = topLevelBlockAnnotations(context.blocks, start, end);
  let cursor = start;

  for (const block of topLevelBlocks) {
    appendTextBlocks(context, blocks, cursor, block.start);
    blocks.push(blockNodeFromAnnotation(context, block));
    cursor = block.end;
  }

  appendTextBlocks(context, blocks, cursor, end);
  return blocks;
}

function topLevelBlockAnnotations(
  blocks: Exclude<MarkupSpan, { type: "span" }>[],
  start: number,
  end: number,
): Exclude<MarkupSpan, { type: "span" }>[] {
  const contained = blocks.filter((block) => block.start >= start && block.end <= end);
  return contained
    .filter((block) => !contained.some((other) =>
      other !== block
      && other.start <= block.start
      && other.end >= block.end
      && (other.start < block.start || other.end > block.end)
    ))
    .sort((a, b) => a.start - b.start || a.end - b.end);
}

function blockNodeFromAnnotation(
  context: ParseContext,
  annotation: Exclude<MarkupSpan, { type: "span" }>,
): BlockNode {
  switch (annotation.type) {
    case "bcode": {
      const text = sliceByCodePoints(context.body, annotation.start, annotation.end);
      return {
        type: "codeBlock",
        attrs: { language: annotation.language ?? null },
        ...(text ? { content: [{ type: "text", text }] } : {}),
      };
    }
    case "bquote":
      return {
        type: "blockquote",
        content: quoteBlocksFromAnnotation(context, annotation),
      };
    case "list":
      return listNodeFromAnnotation(context, annotation);
  }
}

function quoteBlocksFromAnnotation(
  context: ParseContext,
  quote: Extract<MarkupSpan, { type: "bquote" }>,
): BlockNode[] {
  const segments = strippedQuoteSegments(context.body, quote.start, quote.end);
  const content = paragraphsFromSegments(context, segments);
  return content.length > 0 ? content : [{ type: "paragraph" }];
}

function strippedQuoteSegments(body: string, start: number, end: number): TextSegment[] {
  const raw = sliceByCodePoints(body, start, end);
  const lines = raw.split("\n");
  const segments: TextSegment[] = [];
  let lineStart = start;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const marker = line.startsWith("> ") ? 2 : line.startsWith(">") ? 1 : 0;
    if (line.length > marker) {
      segments.push({ text: line.slice(marker), start: lineStart + marker });
    }
    if (i < lines.length - 1) {
      segments.push({ text: "\n", start: lineStart + codePointLength(line) });
    }
    lineStart += codePointLength(line) + 1;
  }
  return segments;
}

function listNodeFromAnnotation(context: ParseContext, list: Extract<MarkupSpan, { type: "list" }>): BlockNode {
  const items = list.items
    .filter((itemStart) => itemStart >= list.start && itemStart < list.end)
    .sort((a, b) => a - b);
  const content = items.map((itemStart, index) => {
    const itemEnd = items[index + 1] ?? list.end;
    return listItemFromRange(context, itemStart, itemEnd);
  });
  const attrs = list.ordered ? orderedListAttrs(context.body, list.start) : undefined;
  return {
    type: list.ordered ? "orderedList" : "bulletList",
    ...(attrs ? { attrs } : {}),
    content,
  };
}

function orderedListAttrs(body: string, start: number): { start: number } | undefined {
  const line = sliceByCodePoints(body, start, Math.min(codePointLength(body), start + 16));
  const match = line.match(/^\s*(\d+)[.)]\s+/);
  if (!match) return undefined;
  const value = Number(match[1]);
  return Number.isFinite(value) && value !== 1 ? { start: value } : undefined;
}

function listItemFromRange(context: ParseContext, start: number, end: number): BlockNode {
  const markerLength = listMarkerLength(context.body, start, end);
  const contentStart = start + markerLength;
  const content = blocksFromRange(context, contentStart, end);
  return {
    type: "listItem",
    content: content.length > 0 ? content : [{ type: "paragraph" }],
  };
}

function listMarkerLength(body: string, start: number, end: number): number {
  const lineEnd = findLineEnd(body, start, end);
  const line = sliceByCodePoints(body, start, lineEnd);
  const match = line.match(/^\s*(?:[-*+]\s+|\d+[.)]\s+)/);
  return match ? codePointLength(match[0]) : 0;
}

function findLineEnd(body: string, start: number, end: number): number {
  const text = sliceByCodePoints(body, start, end);
  const index = text.indexOf("\n");
  return index < 0 ? end : start + codePointLength(text.slice(0, index));
}

function appendTextBlocks(context: ParseContext, blocks: BlockNode[], start: number, end: number): void {
  const range = trimWhitespaceRange(context.body, start, end);
  if (range.end <= range.start) return;
  blocks.push(...paragraphsFromRange(context, range.start, range.end));
}

function trimWhitespaceRange(body: string, start: number, end: number): { start: number; end: number } {
  const chars = Array.from(sliceByCodePoints(body, start, end));
  let left = 0;
  let right = chars.length;
  while (left < right && /\s/.test(chars[left])) left++;
  while (right > left && /\s/.test(chars[right - 1])) right--;
  return { start: start + left, end: start + right };
}

interface TextSegment {
  text: string;
  start: number;
}

function paragraphsFromRange(context: ParseContext, start: number, end: number): BlockNode[] {
  return paragraphsFromSegments(context, [{ text: sliceByCodePoints(context.body, start, end), start }]);
}

function paragraphsFromSegments(context: ParseContext, segments: TextSegment[]): BlockNode[] {
  const paragraphSegments: TextSegment[][] = [];
  let current: TextSegment[] = [];

  for (const segment of splitSegmentsOnBlankLines(segments)) {
    if (segment === null) {
      if (current.length > 0) {
        paragraphSegments.push(current);
        current = [];
      }
      continue;
    }
    current.push(segment);
  }

  if (current.length > 0) paragraphSegments.push(current);

  return paragraphSegments.map((parts) => {
    const content = inlineNodesForSegments(context, parts);
    return content.length > 0 ? { type: "paragraph", content } : { type: "paragraph" };
  });
}

function splitSegmentsOnBlankLines(segments: TextSegment[]): Array<TextSegment | null> {
  const out: Array<TextSegment | null> = [];
  for (const segment of segments) {
    const pieces = segment.text.split(/(\n{2,})/);
    let offset = 0;
    for (const piece of pieces) {
      if (!piece) continue;
      if (/^\n{2,}$/.test(piece)) {
        out.push(null);
      } else {
        out.push({ text: piece, start: segment.start + offset });
      }
      offset += codePointLength(piece);
    }
  }
  return out;
}

function inlineNodesForSegments(context: ParseContext, segments: TextSegment[]): InlineNode[] {
  const nodes: InlineNode[] = [];
  let textBuffer = "";
  let keyBuffer = "";
  let marksBuffer: TiptapMark[] = [];

  const flush = () => {
    if (!textBuffer) return;
    const textNode: InlineNode = { type: "text", text: textBuffer };
    if (marksBuffer.length > 0) textNode.marks = marksBuffer;
    nodes.push(textNode);
    textBuffer = "";
  };

  for (const segment of segments) {
    const chars = Array.from(segment.text);
    for (let i = 0; i < chars.length; i++) {
      const char = chars[i];
      const offset = segment.start + i;
      if (char === "\n") {
        flush();
        nodes.push({ type: "hardBreak" });
        keyBuffer = "";
        marksBuffer = [];
        continue;
      }

      const marks = marksAtOffset(context, offset);
      const key = JSON.stringify(marks);
      if (key !== keyBuffer) {
        flush();
        keyBuffer = key;
        marksBuffer = marks;
      }
      textBuffer += char;
    }
  }
  flush();

  return nodes;
}

function marksAtOffset(context: ParseContext, offset: number): TiptapMark[] {
  const marks: TiptapMark[] = [];
  const span = context.spans.find((candidate) => candidate.start <= offset && offset < candidate.end);
  if (span) {
    for (const style of span.styles) {
      marks.push({ type: MARK_BY_STYLE[style] });
    }
  }

  const reference = context.references.find((candidate) =>
    typeof candidate.begin === "number"
    && typeof candidate.end === "number"
    && candidate.begin <= offset
    && offset < candidate.end
  );
  const href = reference ? safeUri(reference.uri) : null;
  if (href) marks.push({ type: "link", attrs: { href, target: "_blank" } });
  return marks;
}
