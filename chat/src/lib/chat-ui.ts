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

// ── XEP-0393 Message Styling ────────────────────────────────────────

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function parseInlineSpans(input: string): string {
  let result = "";
  const chars = [...input];
  let i = 0;
  let plain = "";

  const flush = () => {
    if (plain) { result += escapeHtml(plain); plain = ""; }
  };

  while (i < chars.length) {
    const ch = chars[i];

    // Inline code
    if (ch === "`") {
      const end = chars.indexOf("`", i + 1);
      if (end !== -1) {
        flush();
        result += `<code class="px-1 bg-muted font-mono text-xs">${escapeHtml(chars.slice(i + 1, end).join(""))}</code>`;
        i = end + 1;
        continue;
      }
    }

    // Styled: * _ ~
    if ((ch === "*" || ch === "_" || ch === "~") && isSpanStart(chars, i)) {
      const end = findClosing(chars, i + 1, ch);
      if (end !== null && isSpanEnd(chars, end)) {
        flush();
        const inner = chars.slice(i + 1, end).join("");
        const tag = ch === "*" ? "strong" : ch === "_" ? "em" : "del";
        result += `<${tag}>${parseInlineSpans(inner)}</${tag}>`;
        i = end + 1;
        continue;
      }
    }

    plain += ch;
    i++;
  }
  flush();
  return result;
}

function findClosing(chars: string[], start: number, ch: string): number | null {
  for (let j = start; j < chars.length; j++) {
    if ((chars[j] ?? "") === ch) return j;
  }
  return null;
}

function isSpanStart(chars: string[], pos: number): boolean {
  if (pos === 0) return true;
  const prev = chars[pos - 1] ?? "";
  return /[\s.,;:!?'"()\[\]{}]/.test(prev);
}

function isSpanEnd(chars: string[], pos: number): boolean {
  if (pos + 1 >= chars.length) return true;
  const next = chars[pos + 1] ?? "";
  return /[\s.,;:!?'"()\[\]{}]/.test(next);
}

/** Render XEP-0393 styled message body to safe HTML. */
export function renderStyledBody(body: string): string {
  const lines = body.split("\n");
  let html = "";
  let i = 0;

  while (i < lines.length) {
    const line = lines[i] ?? "";

    // Code block
    if (line.startsWith("```")) {
      const codeLines: string[] = [];
      i++;
      while (i < lines.length) {
        const cl = lines[i] ?? "";
        if (cl.startsWith("```")) break;
        codeLines.push(cl);
        i++;
      }
      if (i < lines.length) i++; // skip closing ```
      html += `<pre class="bg-muted p-2 text-xs font-mono overflow-x-auto my-1"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
      continue;
    }

    // Block quote
    if (line.startsWith("> ") || line === ">") {
      const quoteLines: string[] = [];
      while (i < lines.length) {
        const ql = lines[i] ?? "";
        if (!ql.startsWith("> ") && ql !== ">") break;
        quoteLines.push(ql === ">" ? "" : ql.slice(2));
        i++;
      }
      html += `<blockquote class="border-l-2 border-muted-foreground pl-3 my-1 text-muted-foreground">${parseInlineSpans(quoteLines.join("\n"))}</blockquote>`;
      continue;
    }

    // Empty line
    if (!line.trim()) { i++; continue; }

    // Normal line
    html += parseInlineSpans(line);
    if (i + 1 < lines.length) html += "<br>";
    i++;
  }

  return html;
}
