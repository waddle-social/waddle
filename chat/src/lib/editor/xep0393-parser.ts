/**
 * XEP-0393 Message Styling → TipTap/ProseMirror JSON converter.
 *
 * Parses a plain-text body using XEP-0393 inline directives and block
 * constructs into a document structure compatible with
 * `editor.commands.setContent()`.
 */

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Mark {
  type: "bold" | "italic" | "strike" | "code" | "link";
  attrs?: Record<string, unknown>;
}

interface TextNode {
  type: "text";
  text: string;
  marks?: Mark[];
}

interface HardBreakNode {
  type: "hardBreak";
}

interface ParagraphNode {
  type: "paragraph";
  content?: (TextNode | HardBreakNode)[];
}

interface CodeBlockNode {
  type: "codeBlock";
  attrs: { language: null };
  content?: TextNode[];
}

interface BlockquoteNode {
  type: "blockquote";
  content: ParagraphNode[];
}

interface ListItemNode {
  type: "listItem";
  content: ParagraphNode[];
}

interface BulletListNode {
  type: "bulletList";
  content: ListItemNode[];
}

interface OrderedListNode {
  type: "orderedList";
  attrs?: { start: number };
  content: ListItemNode[];
}

type BlockNode = ParagraphNode | CodeBlockNode | BlockquoteNode | BulletListNode | OrderedListNode;

interface DocNode {
  type: "doc";
  content: BlockNode[];
}

// ---------------------------------------------------------------------------
// Inline parsing helpers
// ---------------------------------------------------------------------------

const URL_RE = /https?:\/\/[^\s<>"""''()]+/g;

/** A span of text annotated with mark types collected during inline parsing. */
interface InlineSpan {
  text: string;
  bold: boolean;
  italic: boolean;
  strike: boolean;
  code: boolean;
}

/**
 * Returns `true` when `char` at `index` in `text` sits at a valid opening
 * word-boundary for XEP-0393 directives (start-of-string **or** preceded by
 * whitespace).
 */
function isOpeningBoundary(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1]);
}

/**
 * Returns `true` when a closing directive character at `index` sits at a
 * valid closing word-boundary (end-of-string **or** followed by whitespace or
 * punctuation).
 */
function isClosingBoundary(text: string, index: number): boolean {
  return index === text.length - 1 || /[\s.,;:!?\-)\]}>""'']/.test(text[index + 1]);
}

/**
 * Parse a single line of text into {@link InlineSpan}s, handling
 * `code`, then `bold` / `italic` / `strike` directives with word-boundary
 * rules, preserving proper nesting.
 */
function parseInlineSpans(text: string): InlineSpan[] {
  // Phase 1 – extract code spans (backtick delimited). Code spans suppress
  // all other formatting inside them and take highest precedence.
  interface RawSegment {
    text: string;
    isCode: boolean;
  }

  const segments: RawSegment[] = [];
  let pos = 0;

  while (pos < text.length) {
    const tick = text.indexOf("`", pos);
    if (tick === -1) {
      segments.push({ text: text.slice(pos), isCode: false });
      break;
    }
    // Push any text before the backtick.
    if (tick > pos) {
      segments.push({ text: text.slice(pos, tick), isCode: false });
    }
    // Find closing backtick.
    const close = text.indexOf("`", tick + 1);
    if (close === -1) {
      // Unmatched backtick – treat rest as plain text.
      segments.push({ text: text.slice(tick), isCode: false });
      break;
    }
    const inner = text.slice(tick + 1, close);
    if (inner.length > 0) {
      segments.push({ text: inner, isCode: true });
    }
    pos = close + 1;
  }

  // Phase 2 – for each non-code segment, parse bold / italic / strike.
  const spans: InlineSpan[] = [];

  for (const seg of segments) {
    if (seg.isCode) {
      spans.push({
        text: seg.text,
        bold: false,
        italic: false,
        strike: false,
        code: true,
      });
      continue;
    }

    parseDirectives(seg.text, spans);
  }

  return spans;
}

type DirectiveChar = "*" | "_" | "~";
type DirectiveFlag = "bold" | "italic" | "strike";

const DIRECTIVE_MAP: Record<DirectiveChar, DirectiveFlag> = {
  "*": "bold",
  _: "italic",
  "~": "strike",
};

const DIRECTIVES: DirectiveChar[] = ["*", "_", "~"];

/**
 * Recursively parse `*`, `_`, `~` directives out of `text`, appending
 * results into `out`. `activeFlags` tracks which marks are currently open
 * from an outer nesting level.
 */
function parseDirectives(
  text: string,
  out: InlineSpan[],
  activeFlags: Partial<Record<DirectiveFlag, boolean>> = {},
): void {
  if (text.length === 0) return;

  // Find the earliest valid opening directive.
  let bestOpen = -1;
  let bestDir: DirectiveChar | null = null;

  for (const dir of DIRECTIVES) {
    const idx = findOpeningDirective(text, dir);
    if (idx !== -1 && (bestOpen === -1 || idx < bestOpen)) {
      bestOpen = idx;
      bestDir = dir;
    }
  }

  if (bestDir === null || bestOpen === -1) {
    // No directives found – emit plain span.
    pushSpan(out, text, activeFlags);
    return;
  }

  // Find corresponding closing directive.
  const closeIdx = findClosingDirective(text, bestDir, bestOpen + 1);

  if (closeIdx === -1) {
    // No valid close – treat directive char as literal, continue scanning
    // after it.
    pushSpan(out, text.slice(0, bestOpen + 1), activeFlags);
    parseDirectives(text.slice(bestOpen + 1), out, activeFlags);
    return;
  }

  // Content before the opening directive.
  if (bestOpen > 0) {
    pushSpan(out, text.slice(0, bestOpen), activeFlags);
  }

  // Content inside the directive – recurse to handle nesting.
  const inner = text.slice(bestOpen + 1, closeIdx);
  const flag = DIRECTIVE_MAP[bestDir];
  const innerFlags = { ...activeFlags, [flag]: true };
  parseDirectives(inner, out, innerFlags);

  // Content after the closing directive.
  if (closeIdx + 1 < text.length) {
    parseDirectives(text.slice(closeIdx + 1), out, activeFlags);
  }
}

function findOpeningDirective(text: string, dir: DirectiveChar): number {
  let pos = 0;
  while (pos < text.length) {
    const idx = text.indexOf(dir, pos);
    if (idx === -1) return -1;
    if (isOpeningBoundary(text, idx) && idx + 1 < text.length && text[idx + 1] !== dir && !/\s/.test(text[idx + 1])) {
      return idx;
    }
    pos = idx + 1;
  }
  return -1;
}

function findClosingDirective(text: string, dir: DirectiveChar, startAfter: number): number {
  let pos = startAfter;
  while (pos < text.length) {
    const idx = text.indexOf(dir, pos);
    if (idx === -1) return -1;
    // Closing char must not be preceded by whitespace.
    if (idx > 0 && !/\s/.test(text[idx - 1]) && isClosingBoundary(text, idx)) {
      return idx;
    }
    pos = idx + 1;
  }
  return -1;
}

function pushSpan(
  out: InlineSpan[],
  text: string,
  flags: Partial<Record<DirectiveFlag, boolean>>,
): void {
  if (text.length === 0) return;
  out.push({
    text,
    bold: flags.bold ?? false,
    italic: flags.italic ?? false,
    strike: flags.strike ?? false,
    code: false,
  });
}

// ---------------------------------------------------------------------------
// Span → TipTap text-node conversion (including URL auto-linking)
// ---------------------------------------------------------------------------

function spansToNodes(spans: InlineSpan[]): (TextNode | HardBreakNode)[] {
  const nodes: (TextNode | HardBreakNode)[] = [];

  for (const span of spans) {
    const baseMarks: Mark[] = [];
    if (span.code) baseMarks.push({ type: "code" });
    if (span.bold) baseMarks.push({ type: "bold" });
    if (span.italic) baseMarks.push({ type: "italic" });
    if (span.strike) baseMarks.push({ type: "strike" });

    // Code spans don't get auto-linked.
    if (span.code) {
      nodes.push(textNode(span.text, baseMarks));
      continue;
    }

    // Split by URLs and create link-marked nodes.
    let lastIndex = 0;
    URL_RE.lastIndex = 0;
    let match: RegExpExecArray | null;

    while ((match = URL_RE.exec(span.text)) !== null) {
      if (match.index > lastIndex) {
        nodes.push(textNode(span.text.slice(lastIndex, match.index), baseMarks));
      }
      const url = match[0];
      const linkMarks: Mark[] = [
        ...baseMarks,
        { type: "link", attrs: { href: url, target: "_blank" } },
      ];
      nodes.push(textNode(url, linkMarks));
      lastIndex = match.index + url.length;
    }

    if (lastIndex < span.text.length) {
      nodes.push(textNode(span.text.slice(lastIndex), baseMarks));
    }
  }

  return nodes;
}

function textNode(text: string, marks: Mark[]): TextNode {
  const node: TextNode = { type: "text", text };
  if (marks.length > 0) node.marks = marks;
  return node;
}

// ---------------------------------------------------------------------------
// Block-level parsing
// ---------------------------------------------------------------------------

function parseParagraphContent(line: string): (TextNode | HardBreakNode)[] {
  const spans = parseInlineSpans(line);
  return spansToNodes(spans);
}

function makeParagraph(line: string): ParagraphNode {
  const content = parseParagraphContent(line);
  if (content.length === 0) return { type: "paragraph" };
  return { type: "paragraph", content };
}

const BULLET_LIST_RE = /^\s*[-*+]\s+(.*)$/;
const ORDERED_LIST_RE = /^\s*(\d+)[.)]\s+(.*)$/;

function makeListItem(line: string): ListItemNode {
  return { type: "listItem", content: [makeParagraph(line)] };
}

/**
 * Top-level entry: parse an XEP-0393 body into a TipTap-compatible document.
 */
export function parseXep0393ToTiptap(body: string): Record<string, unknown> {
  if (!body || body.length === 0) {
    return { type: "doc", content: [{ type: "paragraph" }] } satisfies DocNode;
  }

  const lines = body.split("\n");
  const blocks: BlockNode[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // --- Code block ---
    if (line.trimEnd() === "```" || line.startsWith("```")) {
      i++;
      const codeLines: string[] = [];
      while (i < lines.length) {
        const cl = lines[i];
        if (cl.trimEnd() === "```") {
          i++;
          break;
        }
        codeLines.push(cl);
        i++;
      }
      const codeText = codeLines.join("\n");
      const node: CodeBlockNode = {
        type: "codeBlock",
        attrs: { language: null },
      };
      if (codeText.length > 0) {
        node.content = [{ type: "text", text: codeText }];
      }
      blocks.push(node);
      continue;
    }

    // --- Blockquote ---
    if (line.startsWith("> ")) {
      const quoteParas: ParagraphNode[] = [];
      while (i < lines.length && lines[i].startsWith("> ")) {
        const stripped = lines[i].slice(2);
        quoteParas.push(makeParagraph(stripped));
        i++;
      }
      blocks.push({ type: "blockquote", content: quoteParas });
      continue;
    }

    const bulletMatch = line.match(BULLET_LIST_RE);
    if (bulletMatch) {
      const items: ListItemNode[] = [];
      while (i < lines.length) {
        const match = lines[i].match(BULLET_LIST_RE);
        if (!match) break;
        items.push(makeListItem(match[1]));
        i++;
      }
      blocks.push({ type: "bulletList", content: items });
      continue;
    }

    const orderedMatch = line.match(ORDERED_LIST_RE);
    if (orderedMatch) {
      const start = Number(orderedMatch[1]);
      const items: ListItemNode[] = [];
      while (i < lines.length) {
        const match = lines[i].match(ORDERED_LIST_RE);
        if (!match) break;
        items.push(makeListItem(match[2]));
        i++;
      }
      blocks.push({
        type: "orderedList",
        ...(start !== 1 ? { attrs: { start } } : {}),
        content: items,
      });
      continue;
    }

    // --- Empty line → empty paragraph ---
    if (line === "") {
      blocks.push({ type: "paragraph" });
      i++;
      continue;
    }

    // --- Normal paragraph ---
    blocks.push(makeParagraph(line));
    i++;
  }

  // Ensure at least one block.
  if (blocks.length === 0) {
    blocks.push({ type: "paragraph" });
  }

  return { type: "doc", content: blocks } satisfies DocNode;
}
