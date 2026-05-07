import type { JSONContent } from "@tiptap/core";
import {
  codePointLength,
  sliceByCodePoints,
} from "@/lib/text-offsets";

export type RichInlineStyle = "strong" | "emphasis" | "deleted" | "code";

export type MarkupSpan =
  | { type: "span"; start: number; end: number; styles: RichInlineStyle[] }
  | { type: "bcode"; start: number; end: number; language?: string }
  | { type: "bquote"; start: number; end: number }
  | { type: "list"; start: number; end: number; ordered: boolean; items: number[] };

export interface MessageReference {
  type: "mention" | "data" | string;
  uri: string;
  begin?: number;
  end?: number;
  anchor?: string;
}

interface RichMessage {
  body: string;
  markup: MarkupSpan[];
  references: MessageReference[];
}

interface TiptapMark {
  type: string;
  attrs?: Record<string, unknown>;
}

interface TiptapNode {
  type?: string;
  content?: TiptapNode[];
  text?: string;
  marks?: TiptapMark[];
  attrs?: Record<string, unknown>;
}

type InlineNode =
  | { type: "text"; text: string; marks?: TiptapMark[] }
  | { type: "hardBreak" };

type BlockNode = JSONContent;

const STYLE_BY_MARK: Record<string, RichInlineStyle> = {
  bold: "strong",
  italic: "emphasis",
  strike: "deleted",
  code: "code",
};

const MARK_BY_STYLE: Record<RichInlineStyle, string> = {
  strong: "bold",
  emphasis: "italic",
  deleted: "strike",
  code: "code",
};

const BLOCK_TYPES = new Set<MarkupSpan["type"]>(["bcode", "bquote", "list"]);

class SerializeState {
  body = "";
  markup: MarkupSpan[] = [];
  references: MessageReference[] = [];

  get offset(): number {
    return codePointLength(this.body);
  }

  append(text: string): void {
    this.body += text;
  }
}

export function tiptapToRichMessage(doc: JSONContent | Record<string, unknown>): RichMessage {
  const state = new SerializeState();
  serializeDoc(state, doc as TiptapNode);
  autolinkifyBareUrls(state);
  return {
    body: state.body,
    markup: normalizeOutboundMarkup(state.markup),
    references: normalizeOutboundReferences(state.references),
  };
}

// Conservative URL pattern. Matches `http(s)://` schemes followed by a run of
// non-whitespace characters, then strips trailing punctuation that is almost
// always sentence-terminal (`.,;:!?` etc.) so `see https://foo.com.` doesn't
// produce a reference that ends on the period.
const BARE_URL_PATTERN = /\bhttps?:\/\/[^\s<>"'`]+/gi;
const TRAILING_PUNCT = /[.,;:!?)\]}'"]+$/;

function autolinkifyBareUrls(state: SerializeState): void {
  if (!state.body) return;

  // Existing reference ranges in the typed projection. URLs already wrapped by
  // a TipTap `link` mark must not be auto-linkified a second time — the mark
  // path wins because it preserves the user's chosen href text.
  const existingRanges: Array<[number, number]> = state.references
    .filter((reference) =>
      typeof reference.begin === "number"
      && typeof reference.end === "number"
      && reference.end > reference.begin
    )
    .map((reference) => [reference.begin as number, reference.end as number]);

  // Code ranges to skip. XEP-0394 `<bcode>` covers fenced code blocks; an
  // inline `<span styles=["code", ...]>` covers backtick code in TipTap.
  const codeRanges: Array<[number, number]> = state.markup
    .filter((span) =>
      span.type === "bcode"
      || (span.type === "span" && span.styles.includes("code"))
    )
    .map((span) => [span.start, span.end]);

  let match: RegExpExecArray | null;
  BARE_URL_PATTERN.lastIndex = 0;
  while ((match = BARE_URL_PATTERN.exec(state.body)) !== null) {
    let raw = match[0];
    raw = raw.replace(TRAILING_PUNCT, "");
    if (!raw) continue;

    const href = safeUri(raw);
    if (!href) continue;

    // codePointLength on the prefix gives a Unicode-scalar offset, matching
    // XEP-0372 begin/end semantics. `match.index` is a UTF-16 code-unit
    // offset and would mis-align after surrogate pairs (emoji, CJK).
    const begin = codePointLength(state.body.slice(0, match.index));
    const end = begin + codePointLength(raw);

    if (rangesOverlap(begin, end, existingRanges)) continue;
    if (rangesOverlap(begin, end, codeRanges)) continue;

    state.references.push({ type: "data", uri: href, begin, end });
    existingRanges.push([begin, end]);
  }
}

function rangesOverlap(begin: number, end: number, ranges: Array<[number, number]>): boolean {
  for (const [rangeBegin, rangeEnd] of ranges) {
    if (begin < rangeEnd && end > rangeBegin) return true;
  }
  return false;
}

function serializeDoc(state: SerializeState, doc: TiptapNode): void {
  serializeBlocks(state, doc.content ?? [], "\n\n");
}

function serializeBlocks(state: SerializeState, nodes: TiptapNode[], separator: string): void {
  let written = 0;
  for (const node of nodes) {
    const before = state.offset;
    if (written > 0) state.append(separator);
    serializeBlock(state, node);
    if (state.offset > before || written > 0) written++;
  }
}

function serializeBlock(state: SerializeState, node: TiptapNode, indent = ""): void {
  switch (node.type) {
    case "paragraph":
      serializeParagraph(state, node, indent);
      break;
    case "codeBlock":
      serializeCodeBlock(state, node, indent);
      break;
    case "blockquote":
      serializeBlockquote(state, node, indent);
      break;
    case "bulletList":
      serializeList(state, node, false, indent);
      break;
    case "orderedList":
      serializeList(state, node, true, indent);
      break;
    case "hardBreak":
      state.append("\n");
      break;
    case "text":
      serializeText(state, node);
      break;
    default:
      break;
  }
}

function serializeParagraph(state: SerializeState, node: TiptapNode, continuationPrefix = ""): void {
  for (const child of node.content ?? []) {
    if (child.type === "hardBreak") {
      state.append("\n");
      if (continuationPrefix) state.append(continuationPrefix);
      continue;
    }
    if (child.type === "text") serializeText(state, child);
  }
}

function serializeText(state: SerializeState, node: TiptapNode): void {
  const text = node.text ?? "";
  if (!text) return;

  const styles = Array.from(
    new Set(
      (node.marks ?? [])
        .map((mark) => STYLE_BY_MARK[mark.type])
        .filter((style): style is RichInlineStyle => !!style),
    ),
  );
  const link = (node.marks ?? []).find((mark) => mark.type === "link");

  const start = state.offset;
  state.append(text);
  const end = state.offset;

  if (styles.length > 0 && end > start) {
    state.markup.push({ type: "span", start, end, styles });
  }

  const href = typeof link?.attrs?.href === "string" ? safeUri(link.attrs.href) : null;
  if (href && end > start) {
    state.references.push({ type: "data", uri: href, begin: start, end });
  }
}

function serializeCodeBlock(state: SerializeState, node: TiptapNode, indent = ""): void {
  if (indent) state.append(indent);
  const start = state.offset;
  state.append(extractPlainText(node));
  const end = state.offset;
  if (end <= start) return;
  const language = typeof node.attrs?.language === "string" ? node.attrs.language.trim() : "";
  state.markup.push({
    type: "bcode",
    start,
    end,
    ...(language ? { language } : {}),
  });
}

function serializeBlockquote(state: SerializeState, node: TiptapNode, indent = ""): void {
  const inner = new SerializeState();
  serializeBlocks(inner, node.content ?? [], "\n\n");
  if (!inner.body) return;

  const start = state.offset;
  appendPrefixedState(state, inner, `${indent}> `);
  const end = state.offset;
  if (end > start) state.markup.push({ type: "bquote", start, end });
}

function appendPrefixedState(outer: SerializeState, inner: SerializeState, prefix: string): void {
  const outerStart = outer.offset;
  const lines = inner.body.split("\n");
  const prefixLength = codePointLength(prefix);
  const lineStarts: number[] = [0];
  let current = 0;
  for (let i = 0; i < lines.length - 1; i++) {
    current += codePointLength(lines[i]) + 1;
    lineStarts.push(current);
  }

  for (let i = 0; i < lines.length; i++) {
    if (i > 0) outer.append("\n");
    outer.append(prefix);
    outer.append(lines[i]);
  }

  const shiftOffset = (offset: number): number => {
    let line = 0;
    for (let i = lineStarts.length - 1; i >= 0; i--) {
      if (offset >= lineStarts[i]) {
        line = i;
        break;
      }
    }
    return outerStart + offset + prefixLength * (line + 1);
  };

  for (const span of inner.markup) {
    outer.markup.push(shiftMarkup(span, shiftOffset));
  }
  for (const reference of inner.references) {
    if (typeof reference.begin !== "number" || typeof reference.end !== "number") continue;
    outer.references.push({
      ...reference,
      begin: shiftOffset(reference.begin),
      end: shiftOffset(reference.end),
    });
  }
}

function shiftMarkup(span: MarkupSpan, shiftOffset: (offset: number) => number): MarkupSpan {
  if (span.type === "list") {
    return {
      ...span,
      start: shiftOffset(span.start),
      end: shiftOffset(span.end),
      items: span.items.map(shiftOffset),
    };
  }
  return { ...span, start: shiftOffset(span.start), end: shiftOffset(span.end) };
}

function serializeList(state: SerializeState, node: TiptapNode, ordered: boolean, indent = ""): void {
  const items = normalizeListItems(node.content ?? []);
  if (items.length === 0) return;

  const listStart = state.offset;
  const itemStarts: number[] = [];
  const startNumber = typeof node.attrs?.start === "number" ? node.attrs.start : 1;

  for (let i = 0; i < items.length; i++) {
    if (i > 0) state.append("\n");
    const marker = ordered ? `${startNumber + i}. ` : "- ";
    itemStarts.push(state.offset);
    state.append(indent + marker);
    serializeListItemContent(state, items[i], `${indent}${" ".repeat(marker.length)}`);
  }

  const listEnd = state.offset;
  if (listEnd > listStart) {
    state.markup.push({ type: "list", start: listStart, end: listEnd, ordered, items: itemStarts });
  }
}

function serializeListItemContent(state: SerializeState, item: TiptapNode, childIndent: string): void {
  const children = item.content ?? [];
  for (let i = 0; i < children.length; i++) {
    const child = children[i];
    if (i > 0 || child.type === "bulletList" || child.type === "orderedList") {
      state.append("\n");
    }

    if (child.type === "paragraph") {
      serializeParagraph(state, child, childIndent);
    } else if (child.type === "bulletList") {
      serializeList(state, child, false, childIndent);
    } else if (child.type === "orderedList") {
      serializeList(state, child, true, childIndent);
    } else {
      serializeBlock(state, child, childIndent);
    }
  }
}

function normalizeListItems(items: TiptapNode[]): TiptapNode[] {
  return items.flatMap((item) => {
    if (item.type !== "listItem") return [];
    const out: TiptapNode[] = [];
    let current: TiptapNode | null = null;
    const ensureCurrent = (): TiptapNode => {
      if (!current) {
        current = { type: "listItem", content: [{ type: "paragraph" }] };
        out.push(current);
      }
      return current;
    };

    for (const child of item.content ?? []) {
      if (child.type === "paragraph") {
        current = { type: "listItem", content: [child] };
        out.push(current);
        continue;
      }
      if (child.type === "bulletList" || child.type === "orderedList") {
        const target = ensureCurrent();
        target.content = [...(target.content ?? []), child];
      }
    }

    return out.length > 0 ? out : [{ type: "listItem", content: [{ type: "paragraph" }] }];
  });
}

function extractPlainText(node: TiptapNode): string {
  if (node.type === "text") return node.text ?? "";
  if (node.type === "hardBreak") return "\n";
  return (node.content ?? []).map(extractPlainText).join("");
}

function normalizeOutboundMarkup(markup: MarkupSpan[]): MarkupSpan[] {
  return markup
    .filter(hasValidMarkupRange)
    .map((span) => span.type === "span" ? { ...span, styles: Array.from(new Set(span.styles)).sort() } : span)
    .sort((a, b) => a.start - b.start || a.end - b.end || a.type.localeCompare(b.type));
}

function normalizeOutboundReferences(references: MessageReference[]): MessageReference[] {
  return references
    .filter((reference) => {
      if (!reference.uri) return false;
      if (typeof reference.begin !== "number" || typeof reference.end !== "number") return false;
      return reference.begin >= 0 && reference.end > reference.begin;
    })
    .sort((a, b) => (a.begin ?? 0) - (b.begin ?? 0) || (a.end ?? 0) - (b.end ?? 0));
}

function hasValidMarkupRange(span: MarkupSpan): boolean {
  if (!Number.isFinite(span.start) || !Number.isFinite(span.end)) return false;
  if (span.start < 0 || span.end <= span.start) return false;
  if (span.type === "span") return span.styles.length > 0;
  if (span.type === "list") {
    return span.items.length > 0 && span.items[0] === span.start && span.items.every((item) => item >= span.start && item < span.end);
  }
  return true;
}

export function richMessageToTiptap(input: {
  body: string;
  markup?: readonly MarkupSpan[];
  references?: readonly MessageReference[];
}): JSONContent {
  const context = createParseContext(input.body, input.markup ?? [], input.references ?? []);
  const content = blocksFromRange(context, 0, context.length);
  return {
    type: "doc",
    content: content.length > 0 ? content : [{ type: "paragraph" }],
  };
}

export function richMessageToMarkdown(input: {
  body: string;
  markup?: readonly MarkupSpan[];
  references?: readonly MessageReference[];
}): string {
  const doc = richMessageToTiptap(input);
  return markdownBlocks((doc.content ?? []) as TiptapNode[], "\n\n");
}

function markdownBlocks(nodes: TiptapNode[], separator: string): string {
  return nodes.map(markdownBlock).filter(Boolean).join(separator);
}

function markdownBlock(node: TiptapNode): string {
  switch (node.type) {
    case "paragraph":
      return markdownInline(node.content ?? []);
    case "codeBlock": {
      const text = extractPlainText(node);
      const language = typeof node.attrs?.language === "string" ? node.attrs.language.trim() : "";
      const fence = markdownFenceFor(text);
      return `${fence}${language}\n${text}\n${fence}`;
    }
    case "blockquote": {
      const inner = markdownBlocks(node.content ?? [], "\n\n");
      return inner.split("\n").map((line) => line ? `> ${line}` : ">").join("\n");
    }
    case "bulletList":
      return markdownList(node, false);
    case "orderedList":
      return markdownList(node, true);
    default:
      return "";
  }
}

function markdownList(node: TiptapNode, ordered: boolean): string {
  const start = typeof node.attrs?.start === "number" ? node.attrs.start : 1;
  return (node.content ?? [])
    .filter((item) => item.type === "listItem")
    .map((item, index) => markdownListItem(item, ordered ? `${start + index}. ` : "- "))
    .join("\n");
}

function markdownListItem(node: TiptapNode, marker: string): string {
  const body = markdownBlocks(node.content ?? [], "\n");
  const lines = body ? body.split("\n") : [""];
  const indent = " ".repeat(marker.length);
  return [marker + lines[0], ...lines.slice(1).map((line) => indent + line)].join("\n");
}

function markdownInline(nodes: TiptapNode[]): string {
  return nodes.map((node) => {
    if (node.type === "hardBreak") return "\n";
    if (node.type !== "text") return "";
    return markdownText(node.text ?? "", node.marks ?? []);
  }).join("");
}

function markdownText(text: string, marks: TiptapMark[]): string {
  const link = marks.find((mark) => mark.type === "link");
  const hasCode = marks.some((mark) => mark.type === "code");
  let out = hasCode ? markdownCodeSpan(text) : escapeMarkdownText(text);

  if (!hasCode) {
    if (marks.some((mark) => mark.type === "bold")) out = `**${out}**`;
    if (marks.some((mark) => mark.type === "italic")) out = `*${out}*`;
    if (marks.some((mark) => mark.type === "strike")) out = `~~${out}~~`;
  }

  const href = typeof link?.attrs?.href === "string" ? safeUri(link.attrs.href) : null;
  if (href) out = `[${out}](${href.replace(/\)/g, "%29")})`;
  return out;
}

function markdownCodeSpan(text: string): string {
  const longestRun = Math.max(0, ...Array.from(text.matchAll(/`+/g)).map((match) => match[0].length));
  const fence = "`".repeat(longestRun + 1);
  const needsPadding = text.startsWith("`") || text.endsWith("`") || text.startsWith(" ") || text.endsWith(" ");
  const value = needsPadding ? ` ${text} ` : text;
  return `${fence}${value}${fence}`;
}

function markdownFenceFor(text: string): string {
  const longestRun = Math.max(2, ...Array.from(text.matchAll(/`+/g)).map((match) => match[0].length));
  return "`".repeat(longestRun + 1);
}

function escapeMarkdownText(text: string): string {
  return text.replace(/[\\`*_{}\[\]()#+\-.!>|~]/g, "\\$&");
}

interface ParseContext {
  body: string;
  length: number;
  markup: MarkupSpan[];
  spans: Extract<MarkupSpan, { type: "span" }>[];
  blocks: Exclude<MarkupSpan, { type: "span" }>[];
  references: MessageReference[];
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

  return {
    body,
    length,
    markup: validMarkup,
    spans: validMarkup.filter((span): span is Extract<MarkupSpan, { type: "span" }> => span.type === "span"),
    blocks: validMarkup.filter((span): span is Exclude<MarkupSpan, { type: "span" }> => BLOCK_TYPES.has(span.type)),
    references: validReferences,
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

function quoteBlocksFromAnnotation(context: ParseContext, quote: Extract<MarkupSpan, { type: "bquote" }>): BlockNode[] {
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

export function renderRichMessageHtml(input: {
  body: string;
  markup?: readonly MarkupSpan[];
  references?: readonly MessageReference[];
}): string {
  const doc = richMessageToTiptap(input);
  return renderBlocks((doc.content ?? []) as TiptapNode[]).trim();
}

function renderBlocks(nodes: TiptapNode[]): string {
  return nodes.map(renderBlock).filter(Boolean).join("");
}

function renderBlock(node: TiptapNode): string {
  switch (node.type) {
    case "paragraph":
      return `<p>${renderInline(node.content ?? [])}</p>`;
    case "codeBlock": {
      const language = typeof node.attrs?.language === "string" && node.attrs.language.trim()
        ? node.attrs.language.trim().toLowerCase()
        : "text";
      return `<pre data-code-block="true" data-language="${escapeHtml(language)}"><code>${escapeHtml(extractPlainText(node))}</code></pre>`;
    }
    case "blockquote":
      return `<blockquote>${renderBlocks(node.content ?? [])}</blockquote>`;
    case "bulletList":
      return `<ul>${(node.content ?? []).map(renderListItem).join("")}</ul>`;
    case "orderedList": {
      const start = typeof node.attrs?.start === "number" && node.attrs.start !== 1
        ? ` start="${node.attrs.start}"`
        : "";
      return `<ol${start}>${(node.content ?? []).map(renderListItem).join("")}</ol>`;
    }
    default:
      return "";
  }
}

function renderListItem(node: TiptapNode): string {
  return `<li>${renderBlocks(node.content ?? [])}</li>`;
}

function renderInline(nodes: TiptapNode[]): string {
  return nodes.map((node) => {
    if (node.type === "hardBreak") return "<br>";
    if (node.type !== "text") return "";
    return renderText(node.text ?? "", node.marks ?? []);
  }).join("");
}

function renderText(text: string, marks: TiptapMark[]): string {
  let html = marks.some((mark) => mark.type === "code")
    ? escapeHtml(text)
    : escapeHtmlWithMentions(text);
  const hasCode = marks.some((mark) => mark.type === "code");
  if (hasCode) html = `<code>${html}</code>`;
  if (marks.some((mark) => mark.type === "bold")) html = `<strong>${html}</strong>`;
  if (marks.some((mark) => mark.type === "italic")) html = `<em>${html}</em>`;
  if (marks.some((mark) => mark.type === "strike")) html = `<s>${html}</s>`;
  const link = marks.find((mark) => mark.type === "link");
  const href = typeof link?.attrs?.href === "string" ? safeUri(link.attrs.href) : null;
  if (href) {
    html = `<a href="${escapeHtml(href)}" target="_blank" rel="noopener noreferrer">${html}</a>`;
  }
  return html;
}

function escapeHtmlWithMentions(text: string): string {
  const pattern = /@(\S+?)(?=[\s<.,;:!?'")\]}&]|$)/g;
  let out = "";
  let cursor = 0;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    out += escapeHtml(text.slice(cursor, match.index));
    out += `<span class="rich-mention">${escapeHtml(match[0])}</span>`;
    cursor = match.index + match[0].length;
  }
  out += escapeHtml(text.slice(cursor));
  return out;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function safeUri(uri: string): string | null {
  try {
    const url = new URL(uri);
    if (!["http:", "https:", "mailto:"].includes(url.protocol)) return null;
    return url.toString();
  } catch {
    return null;
  }
}
