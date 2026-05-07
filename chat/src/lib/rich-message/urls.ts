import { codePointLength } from "@/lib/text-offsets";
import type { MarkupSpan, MessageReference } from "./types";

// Conservative URL pattern. Matches `http(s)://` schemes followed by a run of
// non-whitespace characters; the captured run then has trailing punctuation
// stripped by `stripUrlTrailingPunctuation` below.
const BARE_URL_PATTERN = /\bhttps?:\/\/[^\s<>"'`]+/gi;

export function codeRangesFromMarkup(markup: readonly MarkupSpan[]): Array<[number, number]> {
  return markup
    .filter((span) =>
      span.type === "bcode" || (span.type === "span" && span.styles.includes("code"))
    )
    .map((span) => [span.start, span.end]);
}

export function inferBareUrlReferences(
  body: string,
  references: readonly MessageReference[],
  codeRanges: Array<[number, number]>,
): MessageReference[] {
  // Existing reference ranges in the typed projection. URLs already wrapped by
  // a TipTap `link` mark or an inbound XEP-0372 reference must not be
  // auto-linkified a second time — the explicit reference path wins.
  const existingRanges: Array<[number, number]> = references
    .filter((reference) =>
      typeof reference.begin === "number"
      && typeof reference.end === "number"
      && reference.end > reference.begin
    )
    .map((reference) => [reference.begin as number, reference.end as number]);
  const inferred: MessageReference[] = [];

  let match: RegExpExecArray | null;
  BARE_URL_PATTERN.lastIndex = 0;
  while ((match = BARE_URL_PATTERN.exec(body)) !== null) {
    if (body.slice(Math.max(0, match.index - 2), match.index) === "](") continue;

    const raw = stripUrlTrailingPunctuation(match[0]);
    if (!raw) continue;

    const href = safeUri(raw);
    if (!href) continue;

    // codePointLength on the prefix gives a Unicode-scalar offset, matching
    // XEP-0372 begin/end semantics. `match.index` is a UTF-16 code-unit
    // offset and would mis-align after surrogate pairs (emoji, CJK).
    const begin = codePointLength(body.slice(0, match.index));
    const end = begin + codePointLength(raw);

    if (rangesOverlap(begin, end, existingRanges)) continue;
    if (rangesOverlap(begin, end, codeRanges)) continue;

    inferred.push({ type: "data", uri: href, begin, end });
    existingRanges.push([begin, end]);
  }

  return inferred;
}

function rangesOverlap(begin: number, end: number, ranges: Array<[number, number]>): boolean {
  for (const [rangeBegin, rangeEnd] of ranges) {
    if (begin < rangeEnd && end > rangeBegin) return true;
  }
  return false;
}

// Strip trailing punctuation that is almost always sentence-terminal
// (`.,;:!?'"`) plus *unbalanced* closing brackets. URLs may legitimately end
// in `)` / `]` / `}` (e.g. Wikipedia disambiguation links such as
// `https://en.wikipedia.org/wiki/Foo_(disambiguation)`) — strip the bracket
// only when the captured URL has more closes than opens of that bracket type.
function stripUrlTrailingPunctuation(url: string): string {
  let result = url;
  while (result.length > 0) {
    const ch = result[result.length - 1];
    if (".,;:!?'\"".includes(ch)) {
      result = result.slice(0, -1);
      continue;
    }
    const closingPairs: Array<readonly [string, string]> = [[")", "("], ["]", "["], ["}", "{"]];
    const pair = closingPairs.find(([close]) => close === ch);
    if (pair) {
      const [closeChar, openChar] = pair;
      const closes = countOccurrences(result, closeChar);
      const opens = countOccurrences(result, openChar);
      if (closes > opens) {
        result = result.slice(0, -1);
        continue;
      }
    }
    break;
  }
  return result;
}

function countOccurrences(text: string, char: string): number {
  let count = 0;
  for (let i = 0; i < text.length; i++) if (text[i] === char) count++;
  return count;
}

export function safeUri(uri: string): string | null {
  try {
    const url = new URL(uri);
    if (!["http:", "https:", "mailto:"].includes(url.protocol)) return null;
    return url.toString();
  } catch {
    return null;
  }
}
