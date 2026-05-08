import type { JSONContent } from "@tiptap/core";
import { codePointLength } from "@/lib/text-offsets";
import { extractPlainText } from "./nodes";
import {
  codeRangesFromMarkup,
  inferBareUrlReferences,
  safeUri,
} from "./urls";
import {
  normalizeOutboundMarkup,
  normalizeOutboundReferences,
} from "./validation";
import type {
  MarkupSpan,
  MessageReference,
  RichInlineStyle,
  RichMessage,
  TiptapNode,
} from "./types";
import { STYLE_BY_MARK } from "./types";

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

function autolinkifyBareUrls(state: SerializeState): void {
  if (!state.body) return;

  state.references.push(
    ...inferBareUrlReferences(
      state.body,
      state.references,
      codeRangesFromMarkup(state.markup),
    ),
  );
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
    state.markup.push({
      type: "list",
      start: listStart,
      end: listEnd,
      ordered,
      items: itemStarts,
    });
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
