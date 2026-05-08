import type { JSONContent } from "@tiptap/core";

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

export interface RichMessage {
  body: string;
  markup: MarkupSpan[];
  references: MessageReference[];
}

export interface TiptapMark {
  type: string;
  attrs?: Record<string, unknown>;
}

export interface TiptapNode {
  type?: string;
  content?: TiptapNode[];
  text?: string;
  marks?: TiptapMark[];
  attrs?: Record<string, unknown>;
}

export type InlineNode =
  | { type: "text"; text: string; marks?: TiptapMark[] }
  | { type: "hardBreak" };

export type BlockNode = JSONContent;

export const STYLE_BY_MARK: Record<string, RichInlineStyle> = {
  bold: "strong",
  italic: "emphasis",
  strike: "deleted",
  code: "code",
};

export const MARK_BY_STYLE: Record<RichInlineStyle, string> = {
  strong: "bold",
  emphasis: "italic",
  deleted: "strike",
  code: "code",
};

export const BLOCK_TYPES = new Set<MarkupSpan["type"]>(["bcode", "bquote", "list"]);
