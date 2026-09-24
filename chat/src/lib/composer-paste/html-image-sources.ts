/**
 * Read the `src` of every `<img>` in clipboard `text/html`.
 *
 * Browsers put `<img src="…">` markup on the clipboard for "Copy image".
 * The browser's own `DOMParser` is used when present (it produces an
 * inert document: no scripts run, no images load). Otherwise a small
 * tag scanner handles quoted/unquoted attributes, comments, raw-text
 * elements and character references, so the logic stays testable
 * under `bun test`, which has no DOM.
 */

const RAW_TEXT_ELEMENTS = new Set(["script", "style", "textarea", "title", "xmp", "noscript"]);

const NAMED_CHARACTER_REFERENCES: Readonly<Record<string, string>> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: "\"",
  apos: "'",
  nbsp: " ",
};

interface ScannedTag {
  name: string;
  closing: boolean;
  attributes: Map<string, string>;
  end: number;
}

export function readHtmlImageSources(html: string): string[] {
  if (typeof DOMParser === "function") return parseImageSourcesWithDom(html, new DOMParser());
  return scanHtmlImageSources(html);
}

export function parseImageSourcesWithDom(html: string, parser: DOMParser): string[] {
  const doc = parser.parseFromString(html, "text/html");
  return Array.from(doc.querySelectorAll("img"))
    .map((img) => img.getAttribute("src"))
    .filter((src): src is string => src !== null)
    .map((src) => src.trim());
}

export function scanHtmlImageSources(html: string): string[] {
  const sources: string[] = [];
  let cursor = 0;
  while (cursor < html.length) {
    const open = html.indexOf("<", cursor);
    if (open < 0) break;
    if (html.startsWith("<!--", open)) {
      const close = html.indexOf("-->", open + 4);
      if (close < 0) break;
      cursor = close + 3;
      continue;
    }
    const tag = scanTag(html, open);
    if (!tag) {
      cursor = open + 1;
      continue;
    }
    cursor = tag.end;
    if (tag.closing) continue;
    if (RAW_TEXT_ELEMENTS.has(tag.name)) {
      cursor = indexOfClosingTag(html, tag.name, cursor);
      continue;
    }
    const src = tag.name === "img" ? tag.attributes.get("src") : undefined;
    if (src !== undefined) sources.push(decodeCharacterReferences(src).trim());
  }
  return sources;
}

function scanTag(html: string, open: number): ScannedTag | null {
  let index = open + 1;
  const closing = html[index] === "/";
  if (closing) index += 1;
  if (!isAsciiLetter(html[index])) return null;
  const nameStart = index;
  while (index < html.length && !isTagNameTerminator(html[index])) index += 1;
  const name = html.slice(nameStart, index).toLowerCase();
  const attributes = new Map<string, string>();
  while (index < html.length) {
    index = skipWhile(html, index, (ch) => isWhitespace(ch) || ch === "/");
    if (index >= html.length) return null;
    if (html[index] === ">") return { name, closing, attributes, end: index + 1 };
    const attribute = scanAttribute(html, index);
    if (!attributes.has(attribute.name)) attributes.set(attribute.name, attribute.value);
    index = attribute.end;
  }
  return null;
}

function scanAttribute(html: string, start: number): { name: string; value: string; end: number } {
  let index = start + 1;
  while (index < html.length && !isAttributeNameTerminator(html[index])) index += 1;
  const name = html.slice(start, index).toLowerCase();
  const afterName = skipWhile(html, index, isWhitespace);
  if (html[afterName] !== "=") return { name, value: "", end: index };
  const valueStart = skipWhile(html, afterName + 1, isWhitespace);
  const quote = html[valueStart];
  if (quote === "\"" || quote === "'") {
    const close = html.indexOf(quote, valueStart + 1);
    const end = close < 0 ? html.length : close;
    return { name, value: html.slice(valueStart + 1, end), end: Math.min(end + 1, html.length) };
  }
  const end = skipWhile(html, valueStart, (ch) => !isWhitespace(ch) && ch !== ">");
  return { name, value: html.slice(valueStart, end), end };
}

function indexOfClosingTag(html: string, name: string, from: number): number {
  const close = html.toLowerCase().indexOf(`</${name}`, from);
  return close < 0 ? html.length : close;
}

function decodeCharacterReferences(value: string): string {
  let out = "";
  let cursor = 0;
  while (cursor < value.length) {
    const amp = value.indexOf("&", cursor);
    if (amp < 0) break;
    const semicolon = value.indexOf(";", amp + 1);
    const decoded = semicolon > amp && semicolon - amp <= 12
      ? decodeCharacterReference(value.slice(amp + 1, semicolon))
      : null;
    out += value.slice(cursor, amp) + (decoded ?? "&");
    cursor = decoded === null ? amp + 1 : semicolon + 1;
  }
  return out + value.slice(cursor);
}

function decodeCharacterReference(reference: string): string | null {
  if (reference.startsWith("#x") || reference.startsWith("#X")) {
    return codePointToString(reference.slice(2), 16);
  }
  if (reference.startsWith("#")) return codePointToString(reference.slice(1), 10);
  return NAMED_CHARACTER_REFERENCES[reference] ?? null;
}

function codePointToString(digits: string, radix: 10 | 16): string | null {
  const pattern = radix === 16 ? /^[0-9a-f]+$/i : /^[0-9]+$/;
  if (!pattern.test(digits)) return null;
  const codePoint = Number.parseInt(digits, radix);
  if (codePoint === 0 || codePoint > 0x10ffff) return null;
  return String.fromCodePoint(codePoint);
}

function skipWhile(html: string, from: number, predicate: (ch: string) => boolean): number {
  let index = from;
  while (index < html.length && predicate(html[index])) index += 1;
  return index;
}

function isAsciiLetter(ch: string | undefined): boolean {
  return ch !== undefined && ((ch >= "a" && ch <= "z") || (ch >= "A" && ch <= "Z"));
}

function isWhitespace(ch: string): boolean {
  return ch === " " || ch === "\t" || ch === "\n" || ch === "\r" || ch === "\f";
}

function isTagNameTerminator(ch: string): boolean {
  return isWhitespace(ch) || ch === "/" || ch === ">";
}

function isAttributeNameTerminator(ch: string): boolean {
  return isWhitespace(ch) || ch === "/" || ch === ">" || ch === "=";
}
