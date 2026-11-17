import { roomJidFromJoinUri } from "@/lib/room-links";
import { extractPlainText } from "./nodes";
import { richMessageToTiptap } from "./parse";
import { safeUri } from "./urls";
import type { BlockNode, MarkupSpan, MessageReference, TiptapMark, TiptapNode } from "./types";

/**
 * Display-only inline node carrying the XEP-0245 `/me` actor label
 * ("* Name"). Rendered as escaped plain text — never mention-highlighted —
 * and never produced by parsing a message, only by the `/me` renderer.
 */
export const ME_ACTOR_NODE_TYPE = "meActor";

type RoomHref = (roomJid: string) => string | null;

export function renderRichMessageHtml(input: {
  body: string;
  markup?: readonly MarkupSpan[];
  references?: readonly MessageReference[];
  roomHref?: RoomHref;
}): string {
  return renderRichDocHtml(richMessageToTiptap(input), input.roomHref);
}

export function renderRichDocHtml(doc: BlockNode, roomHref?: RoomHref): string {
  return renderBlocks((doc.content ?? []) as TiptapNode[], roomHref).trim();
}

function renderBlocks(nodes: TiptapNode[], roomHref?: RoomHref): string {
  return nodes.map((node) => renderBlock(node, roomHref)).filter(Boolean).join("");
}

function renderBlock(node: TiptapNode, roomHref?: RoomHref): string {
  switch (node.type) {
    case "paragraph":
      return `<p>${renderInline(node.content ?? [], roomHref)}</p>`;
    case "codeBlock": {
      const language = typeof node.attrs?.language === "string" && node.attrs.language.trim()
        ? node.attrs.language.trim().toLowerCase()
        : "text";
      return `<pre data-code-block="true" data-language="${escapeHtml(language)}"><code>${escapeHtml(extractPlainText(node))}</code></pre>`;
    }
    case "blockquote":
      return `<blockquote>${renderBlocks(node.content ?? [], roomHref)}</blockquote>`;
    case "bulletList":
      return `<ul>${(node.content ?? []).map((item) => renderListItem(item, roomHref)).join("")}</ul>`;
    case "orderedList": {
      const start = typeof node.attrs?.start === "number" && node.attrs.start !== 1
        ? ` start="${node.attrs.start}"`
        : "";
      return `<ol${start}>${(node.content ?? []).map((item) => renderListItem(item, roomHref)).join("")}</ol>`;
    }
    default:
      return "";
  }
}

function renderListItem(node: TiptapNode, roomHref?: RoomHref): string {
  return `<li>${renderBlocks(node.content ?? [], roomHref)}</li>`;
}

function renderInline(nodes: TiptapNode[], roomHref?: RoomHref): string {
  return nodes.map((node) => {
    if (node.type === "hardBreak") return "<br>";
    if (node.type === ME_ACTOR_NODE_TYPE) return `<span class="font-semibold">${escapeHtml(node.text ?? "")}</span>`;
    if (node.type !== "text") return "";
    return renderText(node.text ?? "", node.marks ?? [], roomHref);
  }).join("");
}

function renderText(text: string, marks: TiptapMark[], roomHref?: RoomHref): string {
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
  const roomJid = href ? roomJidFromJoinUri(href) : null;
  if (roomJid) {
    const path = roomHref?.(roomJid);
    return path ? `<a href="${escapeHtml(path)}" data-room-jid="${escapeHtml(roomJid)}">${html}</a>` : html;
  }
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
