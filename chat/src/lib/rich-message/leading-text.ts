import type { JSONContent } from "@tiptap/core";

// Edits at the very start of a rich document (TipTap JSON). Lengths count
// UTF-16 code units of text and one unit per hard break, matching
// `extractPlainText`, so a length measured on a textblock's plain text can
// be fed straight back into `dropLeadingDocText`.

const TEXTBLOCK_TYPES = new Set(["paragraph", "codeBlock"]);

function isTextblock(node: JSONContent): boolean {
  return TEXTBLOCK_TYPES.has(node.type ?? "");
}

function inlineLength(node: JSONContent): number {
  if (node.type === "text") return node.text?.length ?? 0;
  if (node.type === "hardBreak") return 1;
  return 0;
}

function dropLeadingInline(nodes: readonly JSONContent[], count: number): JSONContent[] {
  let remaining = count;
  const kept: JSONContent[] = [];
  for (const node of nodes) {
    if (remaining <= 0) {
      kept.push(node);
      continue;
    }
    const length = inlineLength(node);
    if (length <= remaining) {
      remaining -= length;
      continue;
    }
    kept.push({ ...node, text: (node.text ?? "").slice(remaining) });
    remaining = 0;
  }
  return kept;
}

/** Remove the first `count` characters of the first textblock, keeping marks on what remains. */
export function dropLeadingDocText(node: JSONContent, count: number): JSONContent {
  if (isTextblock(node)) return { ...node, content: dropLeadingInline(node.content ?? [], count) };
  const [first, ...rest] = node.content ?? [];
  if (!first) return node;
  return { ...node, content: [dropLeadingDocText(first, count), ...rest] };
}
