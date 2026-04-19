import type { WaddleEncryptedFile } from "@/lib/xmpp/extensions/encrypted-file";

export type AppState = "loading" | "signed-out" | "ready" | "error";
export type AdminTab = "rooms" | "people" | "settings";
export type EditableRole = "member" | "moderator" | "admin";

/** Delivery status for messages sent by the current user. */
export type DeliveryStatus = "queued" | "sending" | "delivered" | "failed";

export interface MarkupSpan {
  type: "b" | "i" | "s" | "code" | "code-block" | "blockquote" | "link";
  start: number;
  end: number;
  uri?: string;
}

export interface TimelineSharedFile {
  name?: string;
  mediaType?: string;
  size?: number;
  width?: number;
  height?: number;
  desc?: string;
  url: string;
  disposition: "inline" | "attachment";
  encrypted?: WaddleEncryptedFile;
}

export interface TimelineMessage {
  id: string;
  author: string;
  /** Actual JID / occupant JID for the author when known. */
  authorJid?: string;
  body: string;
  createdAt: string;
  isSelf: boolean;
  /** Delivery status — only meaningful for self-sent messages. */
  deliveryStatus?: DeliveryStatus;
  /** Whether this message has been edited (XEP-0308). */
  isEdited?: boolean;
  /** Whether this message has been retracted (XEP-0424). */
  isRetracted?: boolean;
  /** Aggregated emoji reactions: emoji -> list of nicks (XEP-0444). */
  reactions?: Record<string, string[]>;
  /** Users who have seen this message (XEP-0333). */
  readBy?: string[];
  /** Mentioned JIDs/nicks in this message (XEP-0372). */
  mentions?: string[];
  /** XEP-0317: Hat badges for the message author. */
  hats?: { title: string; uri: string }[];
  /** XEP-0449: Is this a sticker message? */
  isSticker?: boolean;
  /** XEP-0446/0447: Shared files (zero or more attachments). */
  sharedFiles?: TimelineSharedFile[];
  /** XEP-0513: Broadcast mention (everyone/here). */
  broadcastMention?: "everyone" | "here";
  /** XEP-0394: Message Markup offset-based annotations. */
  markup?: MarkupSpan[];
  /** XEP-0461: Parent message this replies to, with optional preview text. */
  replyTo?: { id: string; author?: string; preview?: string };
  /** RFC 6121 / XEP-0201: Thread identifier + optional parent thread. */
  threadId?: string;
  parentThreadId?: string;
  /** XEP-0508: Forum topic/reply metadata when present. */
  forumPostKind?: "topic" | "reply";
  forumTitle?: string;
  forumThreadTitle?: string;
}

export interface CommunityFormData {
  name: string;
  description: string;
  is_public: boolean;
}

export interface ChannelCreateFormData {
  name: string;
  description: string;
  channel_type: string;
  position: number;
}

export interface ChannelEditFormData {
  name: string;
  description: string;
  position: number;
}

// ── XEP-0392 Consistent Color Generation ────────────────────────────

/** Compute a deterministic hue (0–360) from a string using XEP-0392 algorithm. */
export function consistentHue(input: string): number {
  // Simple SHA-1-like hash using SubtleCrypto isn't available synchronously.
  // Use the same algorithm: hash → first 2 bytes → hue mapping.
  // We use a simple DJB2-variant that gives good distribution for short strings.
  let h = 5381;
  for (let i = 0; i < input.length; i++) {
    h = ((h << 5) + h + input.charCodeAt(i)) & 0xffff;
  }
  return (h / 65536) * 360;
}

/** Generate a CSS hsl() color string for a username/JID. */
export function consistentColor(input: string, saturation = 65, lightness = 50): string {
  const hue = consistentHue(input);
  return `hsl(${Math.round(hue)}, ${saturation}%, ${lightness}%)`;
}

// ── XEP-0393 Message Styling (via marked) ───────────────────────────

import { Marked } from "marked";

/**
 * XEP-0393 defines `*text*` as bold (not italic like standard Markdown)
 * and `~text~` as strikethrough (standard GFM uses `~~text~~`).
 *
 * We pre-process the text to map XEP-0393 syntax to standard GFM before
 * passing it to `marked` for rendering.
 */

const WB = /[\s.,;:!?'"()\[\]{}]/;
const encoder = new TextEncoder();

/** Convert XEP-0393 body text to GitHub-Flavored Markdown. */
function xep0393ToGfm(input: string): string {
  const lines = input.split("\n");
  let out = "";
  let inCode = false;

  for (const line of lines) {
    if (line.startsWith("```")) {
      inCode = !inCode;
      out += line + "\n";
      continue;
    }
    if (inCode) {
      out += line + "\n";
      continue;
    }
    out += convertInline(line) + "\n";
  }

  // Remove trailing newline added by loop
  return out.slice(0, -1);
}

/** Convert XEP-0393 inline styling to GFM inline styling within a single line. */
function convertInline(line: string): string {
  const chars = [...line];
  let result = "";
  let i = 0;

  while (i < chars.length) {
    const ch = chars[i]!;

    // Skip inline code spans — pass through unchanged
    if (ch === "`") {
      const end = chars.indexOf("`", i + 1);
      if (end !== -1) {
        result += chars.slice(i, end + 1).join("");
        i = end + 1;
        continue;
      }
    }

    // *text* → **text** (XEP-0393 bold → GFM bold)
    if (ch === "*" && isStart(chars, i)) {
      const end = findClose(chars, i + 1, "*");
      if (end !== null && isEnd(chars, end)) {
        const inner = chars.slice(i + 1, end).join("");
        result += `**${convertInline(inner)}**`;
        i = end + 1;
        continue;
      }
    }

    // ~text~ → ~~text~~ (XEP-0393 strikethrough → GFM strikethrough)
    if (ch === "~" && isStart(chars, i)) {
      const end = findClose(chars, i + 1, "~");
      if (end !== null && isEnd(chars, end)) {
        const inner = chars.slice(i + 1, end).join("");
        result += `~~${convertInline(inner)}~~`;
        i = end + 1;
        continue;
      }
    }

    result += ch;
    i++;
  }
  return result;
}

function findClose(chars: string[], start: number, ch: string): number | null {
  for (let j = start; j < chars.length; j++) {
    if (chars[j] === ch) return j;
  }
  return null;
}

function isStart(chars: string[], pos: number): boolean {
  if (pos === 0) return true;
  return WB.test(chars[pos - 1]!);
}

function isEnd(chars: string[], pos: number): boolean {
  if (pos + 1 >= chars.length) return true;
  return WB.test(chars[pos + 1]!);
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function byteLen(text: string): number {
  return encoder.encode(text).byteLength;
}

/** Tailwind-styled marked instance for XEP-0393 rendering. */
const md = new Marked({
  gfm: true,
  breaks: true,
  renderer: {
    code({ text, lang }: { text: string; lang?: string }) {
      const normalizedLang = (lang ?? "").trim().toLowerCase();
      const safeLang = normalizedLang ? escapeHtml(normalizedLang) : "text";
      return `<pre data-code-block="true" data-language="${safeLang}" class="bg-muted p-2 text-xs font-mono overflow-x-auto my-1"><code>${escapeHtml(text)}</code></pre>`;
    },
    codespan({ text }: { text: string }) {
      return `<code class="px-1 bg-muted font-mono text-xs">${escapeHtml(text)}</code>`;
    },
    blockquote({ text }: { text: string }) {
      return `<blockquote class="border-l-2 border-muted-foreground pl-3 my-1 text-muted-foreground">${text}</blockquote>`;
    },
    paragraph(token) {
      return this.parser.parseInline(token.tokens);
    },
    // Features not in XEP-0393 — de-style to plain text (preserve content, strip formatting)
    heading({ text }: { text: string }) { return text; },
    hr() { return ""; },
    list(token) { return token.items.map((i) => i.text).join("\n"); },
    listitem({ text }: { text: string }) { return `${text}\n`; },
    image() { return ""; },
    link({ href, text }: { href: string; text: string }) {
      try {
        const url = new URL(href);
        if (!["http:", "https:", "mailto:"].includes(url.protocol)) return text;
      } catch {
        return text;
      }
      return `<a href="${escapeHtml(href)}" target="_blank" rel="noopener noreferrer" class="text-blue-500 underline hover:text-blue-400">${text}</a>`;
    },
    // Drop raw HTML tokens so untrusted input cannot inject markup
    html() { return ""; },
  },
});

function hasRichFormattingMarkers(body: string): boolean {
  if (!body) return false;
  // Body directives are authoritative when present because they preserve
  // intended formatting without needing offset reinterpretation.
  return /```|^>\s|[`*_~]/m.test(body);
}

interface RenderNormalizationResult {
  canonicalBody: string;
  source: "body" | "markup-synthesized";
}

interface MarkerWrapper {
  open: string;
  close: string;
}

function isSafeLinkUri(uri: string): string | null {
  try {
    const url = new URL(uri);
    if (!["http:", "https:", "mailto:"].includes(url.protocol)) return null;
    return url.toString();
  } catch {
    return null;
  }
}

function wrapperForSpan(span: MarkupSpan): MarkerWrapper | null {
  switch (span.type) {
    case "b":
      return { open: "*", close: "*" };
    case "i":
      return { open: "_", close: "_" };
    case "s":
      return { open: "~", close: "~" };
    case "code":
      return { open: "`", close: "`" };
    case "code-block":
      return { open: "```\n", close: "\n```" };
    case "blockquote":
      return { open: "> ", close: "" };
    case "link": {
      const safeUri = span.uri ? isSafeLinkUri(span.uri) : null;
      if (!safeUri) return null;
      return { open: "[", close: `](<${safeUri}>)` };
    }
  }
}

function byteOffsetToCodeUnitIndex(input: string, targetOffset: number): number {
  if (targetOffset <= 0) return 0;
  let offset = 0;
  let codeUnitIndex = 0;
  for (const ch of input) {
    if (offset >= targetOffset) break;
    const next = offset + byteLen(ch);
    if (next > targetOffset) {
      return codeUnitIndex;
    }
    offset = next;
    codeUnitIndex += ch.length;
  }
  return codeUnitIndex;
}

function synthesizeBodyFromMarkup(body: string, markup: MarkupSpan[]): string {
  if (markup.length === 0) return body;
  const totalBytes = byteLen(body);

  type MarkerEvent = {
    index: number;
    kind: "open" | "close";
    text: string;
    spanLen: number;
  };

  const events: MarkerEvent[] = [];
  for (const span of markup) {
    if (span.start < 0 || span.end <= span.start || span.end > totalBytes) continue;
    const wrapper = wrapperForSpan(span);
    if (!wrapper) continue;

    const startIndex = byteOffsetToCodeUnitIndex(body, span.start);
    const endIndex = byteOffsetToCodeUnitIndex(body, span.end);
    if (endIndex <= startIndex) continue;

    const spanLen = span.end - span.start;
    events.push({ index: startIndex, kind: "open", text: wrapper.open, spanLen });
    events.push({ index: endIndex, kind: "close", text: wrapper.close, spanLen });
  }

  if (events.length === 0) return body;

  events.sort((a, b) => {
    if (a.index !== b.index) return a.index - b.index;
    if (a.kind !== b.kind) return a.kind === "close" ? -1 : 1;
    if (a.kind === "open") return b.spanLen - a.spanLen;
    return a.spanLen - b.spanLen;
  });

  let out = "";
  let cursor = 0;
  let i = 0;

  while (i < events.length) {
    const idx = events[i].index;
    out += body.slice(cursor, idx);
    while (i < events.length && events[i].index === idx) {
      out += events[i].text;
      i++;
    }
    cursor = idx;
  }

  out += body.slice(cursor);
  return out;
}

function normalizeRenderBody(body: string, markup?: MarkupSpan[]): RenderNormalizationResult {
  if (!markup || markup.length === 0) {
    return { canonicalBody: body, source: "body" };
  }
  if (hasRichFormattingMarkers(body)) {
    return { canonicalBody: body, source: "body" };
  }
  return {
    canonicalBody: synthesizeBodyFromMarkup(body, markup),
    source: "markup-synthesized",
  };
}

/** Render XEP-0393 styled message body to safe HTML through one canonical pipeline. */
export function renderStyledBody(body: string, markup?: MarkupSpan[]): string {
  const normalized = normalizeRenderBody(body, markup);
  const gfm = xep0393ToGfm(normalized.canonicalBody);
  const html = (md.parse(gfm) as string).trim();

  // Style @mentions — match @non-whitespace after rendered HTML
  return html.replace(
    /(?:^|(?<=\s|>))@(\S+?)(?=[\s<.,;:!?'")\]}&]|$)/g,
    '<span class="text-blue-500 font-bold">@$1</span>',
  );
}

// ── Image / GIF URL detection ────────────────────────────────────────

const IMAGE_URL_RE = /^https?:\/\/\S+\.(?:gif|png|jpe?g|webp)(?:\?\S*)?$/i;
const GIPHY_URL_RE = /^https?:\/\/(?:media\d*\.giphy\.com|i\.giphy\.com)\//i;

/** Check if a message body is a single image/GIF URL that should render inline. */
export function isImageUrl(body: string): boolean {
  const trimmed = body.trim();
  return IMAGE_URL_RE.test(trimmed) || GIPHY_URL_RE.test(trimmed);
}
