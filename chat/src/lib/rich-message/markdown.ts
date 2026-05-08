import { extractPlainText } from "./nodes";
import { richMessageToTiptap } from "./parse";
import { safeUri } from "./urls";
import type { MarkupSpan, MessageReference, TiptapMark, TiptapNode } from "./types";

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
