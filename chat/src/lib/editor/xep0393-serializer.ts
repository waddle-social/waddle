/**
 * Serializes a TipTap/ProseMirror document into XEP-0393 styled plain text
 * and produces XEP-0394 markup span annotations.
 *
 * XEP-0393: Message Styling — inline directives in the plain text body
 * XEP-0394: Message Markup — offset-based annotations pointing into the body
 */

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export interface MarkupSpan {
  type: 'b' | 'i' | 's' | 'code' | 'code-block' | 'blockquote' | 'link';
  start: number;
  end: number;
  uri?: string;
}

export interface SerializedMessage {
  /** XEP-0393 formatted plain text */
  body: string;
  /** XEP-0394 annotations */
  markup: MarkupSpan[];
}

// ---------------------------------------------------------------------------
// Internal TipTap JSON shapes (loose — we only read what we need)
// ---------------------------------------------------------------------------

interface TiptapMark {
  type: string;
  attrs?: Record<string, unknown>;
}

interface TiptapNode {
  type: string;
  content?: TiptapNode[];
  text?: string;
  marks?: TiptapMark[];
  attrs?: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const encoder = new TextEncoder();

/** Return the byte length of a UTF-8 string. */
function byteLen(s: string): number {
  return encoder.encode(s).byteLength;
}

const MARK_TO_DIRECTIVE: Record<string, { open: string; close: string; spanType: MarkupSpan['type'] }> = {
  bold: { open: '*', close: '*', spanType: 'b' },
  italic: { open: '_', close: '_', spanType: 'i' },
  strike: { open: '~', close: '~', spanType: 's' },
  code: { open: '`', close: '`', spanType: 'code' },
};

// ---------------------------------------------------------------------------
// Serializer state — accumulates body text and spans
// ---------------------------------------------------------------------------

class SerializerState {
  body = '';
  /** Current byte offset into `body`. */
  offset = 0;
  markup: MarkupSpan[] = [];

  /** Append a raw string, advancing the byte offset. */
  append(s: string): void {
    this.body += s;
    this.offset += byteLen(s);
  }
}

// ---------------------------------------------------------------------------
// Node walkers
// ---------------------------------------------------------------------------

function serializeDoc(state: SerializerState, node: TiptapNode): void {
  if (!node.content) return;
  serializeNodes(state, node.content, '\n');
}

/**
 * Serialize an array of block-level nodes, inserting `sep` between them.
 */
function serializeNodes(state: SerializerState, nodes: TiptapNode[], sep: string): void {
  for (let i = 0; i < nodes.length; i++) {
    if (i > 0) state.append(sep);
    serializeBlock(state, nodes[i]);
  }
}

function serializeBlock(state: SerializerState, node: TiptapNode): void {
  switch (node.type) {
    case 'doc':
      serializeDoc(state, node);
      break;
    case 'paragraph':
      serializeParagraph(state, node);
      break;
    case 'codeBlock':
      serializeCodeBlock(state, node);
      break;
    case 'blockquote':
      serializeBlockquote(state, node);
      break;
    case 'bulletList':
      serializeBulletList(state, node);
      break;
    case 'orderedList':
      serializeOrderedList(state, node);
      break;
    case 'listItem':
      serializeListItem(state, node);
      break;
    case 'hardBreak':
      state.append('\n');
      break;
    case 'image':
      serializeImage(state, node);
      break;
    case 'text':
      serializeText(state, node);
      break;
    default:
      // Unknown block — try to recurse into its content
      if (node.content) {
        serializeNodes(state, node.content, '\n');
      }
      break;
  }
}

// ---------------------------------------------------------------------------
// Inline content (text with marks)
// ---------------------------------------------------------------------------

function serializeParagraph(state: SerializerState, node: TiptapNode): void {
  if (!node.content) return; // empty paragraph
  serializeInlineNodes(state, node.content);
}

function serializeInlineNodes(state: SerializerState, nodes: TiptapNode[]): void {
  for (const child of nodes) {
    if (child.type === 'hardBreak') {
      state.append('\n');
    } else if (child.type === 'image') {
      serializeImage(state, child);
    } else if (child.type === 'text') {
      serializeText(state, child);
    } else if (child.content) {
      // Unexpected inline node with children — recurse
      serializeInlineNodes(state, child.content);
    }
  }
}

function serializeText(state: SerializerState, node: TiptapNode): void {
  const text = node.text ?? '';
  if (text === '') return;

  const marks = node.marks ?? [];

  // Separate link marks from styling marks so we can record the URI
  const stylingMarks: TiptapMark[] = [];
  const linkMarks: TiptapMark[] = [];

  for (const m of marks) {
    if (m.type === 'link') {
      linkMarks.push(m);
    } else if (MARK_TO_DIRECTIVE[m.type]) {
      stylingMarks.push(m);
    }
  }

  // Open directive characters (outermost → innermost)
  const opens: { directive: typeof MARK_TO_DIRECTIVE[string]; startOffset: number }[] = [];
  for (const m of stylingMarks) {
    const dir = MARK_TO_DIRECTIVE[m.type]!;
    const startOffset = state.offset;
    state.append(dir.open);
    opens.push({ directive: dir, startOffset });
  }

  // Record link span start (links have no directive chars in XEP-0393)
  const linkStartOffset = state.offset;

  // Emit the actual text
  state.append(text);

  // Record link spans
  for (const lm of linkMarks) {
    const uri = (lm.attrs?.href as string) ?? '';
    state.markup.push({
      type: 'link',
      start: linkStartOffset,
      end: state.offset,
      uri,
    });
  }

  // Close directive characters (innermost → outermost, i.e. reverse order)
  for (let i = opens.length - 1; i >= 0; i--) {
    const { directive, startOffset } = opens[i];
    state.append(directive.close);
    state.markup.push({
      type: directive.spanType,
      start: startOffset,
      end: state.offset,
    });
  }
}

// ---------------------------------------------------------------------------
// Code blocks
// ---------------------------------------------------------------------------

function serializeCodeBlock(state: SerializerState, node: TiptapNode): void {
  const startOffset = state.offset;
  state.append('```\n');

  const text = extractPlainText(node);
  state.append(text);

  // Ensure closing fence is on its own line
  if (text.length > 0 && !text.endsWith('\n')) {
    state.append('\n');
  }
  state.append('```');

  state.markup.push({
    type: 'code-block',
    start: startOffset,
    end: state.offset,
  });
}

// ---------------------------------------------------------------------------
// Blockquotes
// ---------------------------------------------------------------------------

function serializeBlockquote(state: SerializerState, node: TiptapNode): void {
  if (!node.content) return;

  const startOffset = state.offset;

  // Serialize inner content to a temporary state, then prefix each line with `> `
  const inner = new SerializerState();
  serializeNodes(inner, node.content, '\n');

  const lines = inner.body.split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (i > 0) state.append('\n');
    state.append('> ' + lines[i]);
  }

  // Re-map inner markup spans into the outer state's offset space.
  // We need to account for the `> ` prefixes injected at each line start.
  remapInnerSpans(state, inner, startOffset, lines);

  state.markup.push({
    type: 'blockquote',
    start: startOffset,
    end: state.offset,
  });
}

/**
 * Remap spans from an inner SerializerState into the outer state,
 * accounting for per-line prefixes that shift byte offsets.
 *
 * `lines` are the split lines of the inner body (before prefixing).
 * The outer body has `> ` (2 bytes) prepended to each line, plus `\n`
 * between lines.
 */
function remapInnerSpans(
  outer: SerializerState,
  inner: SerializerState,
  outerStart: number,
  lines: string[],
): void {
  // Build a map: for each byte offset in the inner body, compute the
  // additional shift caused by the `> ` prefixes.
  // Line boundaries in the inner body (byte offsets of each `\n`).
  const prefixBytes = byteLen('> ');
  const lineByteStarts: number[] = [0];
  let acc = 0;
  for (let i = 0; i < lines.length - 1; i++) {
    acc += byteLen(lines[i]) + 1; // +1 for the `\n`
    lineByteStarts.push(acc);
  }

  function shiftOffset(innerOffset: number): number {
    // Find which line this offset falls on
    let line = 0;
    for (let l = lineByteStarts.length - 1; l >= 0; l--) {
      if (innerOffset >= lineByteStarts[l]) {
        line = l;
        break;
      }
    }
    // Each line before and including this one has an extra `> ` prefix
    // Plus `\n` separators are already in the inner body
    const shift = (line + 1) * prefixBytes;
    return outerStart + innerOffset + shift;
  }

  for (const span of inner.markup) {
    outer.markup.push({
      ...span,
      start: shiftOffset(span.start),
      end: shiftOffset(span.end),
    });
  }
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

function serializeBulletList(state: SerializerState, node: TiptapNode): void {
  if (!node.content) return;
  for (let i = 0; i < node.content.length; i++) {
    if (i > 0) state.append('\n');
    state.append('- ');
    serializeListItemContent(state, node.content[i]);
  }
}

function serializeOrderedList(state: SerializerState, node: TiptapNode): void {
  if (!node.content) return;
  const startNum = (node.attrs?.start as number) ?? 1;
  for (let i = 0; i < node.content.length; i++) {
    if (i > 0) state.append('\n');
    state.append(`${startNum + i}. `);
    serializeListItemContent(state, node.content[i]);
  }
}

function serializeListItemContent(state: SerializerState, node: TiptapNode): void {
  if (!node.content) return;
  // List items usually contain paragraphs; join them with newlines
  for (let i = 0; i < node.content.length; i++) {
    if (i > 0) state.append('\n');
    serializeBlock(state, node.content[i]);
  }
}

function serializeListItem(state: SerializerState, node: TiptapNode): void {
  // Standalone listItem outside of a list context — just serialize content
  serializeListItemContent(state, node);
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

function serializeImage(state: SerializerState, node: TiptapNode): void {
  const src = (node.attrs?.src as string) ?? '';
  if (src) {
    state.append(src);
  }
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/** Extract plain text from a node tree (used for code blocks). */
function extractPlainText(node: TiptapNode): string {
  if (node.type === 'text') return node.text ?? '';
  if (node.type === 'hardBreak') return '\n';
  if (!node.content) return '';
  return node.content.map(extractPlainText).join('');
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

export function serializeTiptapToXep0393(doc: TiptapNode): SerializedMessage {
  const state = new SerializerState();
  serializeDoc(state, doc);
  return { body: state.body, markup: state.markup };
}
