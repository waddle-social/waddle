import { extractPlainText } from "./nodes";
import { richMessageToTiptap } from "./parse";
import { safeUri } from "./urls";
import type { MarkupSpan, MessageReference, TiptapMark, TiptapNode } from "./types";

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
