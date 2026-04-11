export type AppState = "loading" | "signed-out" | "ready" | "error";
export type AdminTab = "rooms" | "people" | "settings";
export type EditableRole = "member" | "moderator" | "admin";

/** Delivery status for messages sent by the current user. */
export type DeliveryStatus = "sending" | "delivered";

export interface TimelineMessage {
  id: string;
  author: string;
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
  /** XEP-0513: Broadcast mention (everyone/here). */
  broadcastMention?: "everyone" | "here";
  /** XEP-0482/0483: Call invite info. */
  callInvite?: { sessionId: string; audio: boolean; video: boolean; externalUri?: string; meetingDesc?: string };
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

/** Tailwind-styled marked instance for XEP-0393 rendering. */
const md = new Marked({
  gfm: true,
  breaks: true,
  renderer: {
    code({ text }: { text: string }) {
      return `<pre class="bg-muted p-2 text-xs font-mono overflow-x-auto my-1"><code>${escapeHtml(text)}</code></pre>`;
    },
    codespan({ text }: { text: string }) {
      return `<code class="px-1 bg-muted font-mono text-xs">${escapeHtml(text)}</code>`;
    },
    blockquote({ text }: { text: string }) {
      return `<blockquote class="border-l-2 border-muted-foreground pl-3 my-1 text-muted-foreground">${text}</blockquote>`;
    },
    paragraph({ text }: { text: string }) {
      return text;
    },
    // Features not in XEP-0393 — de-style to plain text (preserve content, strip formatting)
    heading({ text }: { text: string }) { return text; },
    hr() { return ""; },
    list(token) { return token.items.map((i) => i.text).join("\n"); },
    listitem({ text }: { text: string }) { return `${text}\n`; },
    image() { return ""; },
    link({ text }: { text: string }) { return text; },
    // Drop raw HTML tokens so untrusted input cannot inject markup
    html() { return ""; },
  },
});

/** Render XEP-0393 styled message body to safe HTML. */
export function renderStyledBody(body: string): string {
  const gfm = xep0393ToGfm(body);
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
