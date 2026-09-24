import type { JSONContent } from "@tiptap/core";
import { extractPlainText } from "./rich-message/nodes";
import { dropLeadingDocText } from "./rich-message/leading-text";
import type { TiptapNode } from "./rich-message/types";
import { ME_COMMAND_PREFIX } from "./me-command";
import type { BuiltinSendRewrite } from "./slash-builtins";

export const SHRUG = "¯\\_(ツ)_/¯";

/** The leading `/word` plus the whitespace after it (mirrors `parseSlashTrigger`). */
const LEADING_COMMAND_PATTERN = /^\/[a-zA-Z][a-zA-Z0-9_-]*\s*/;

function plainText(node: JSONContent): string {
  return extractPlainText(node as TiptapNode);
}

function isEmptyParagraph(node: JSONContent | undefined): boolean {
  return node?.type === "paragraph" && plainText(node) === "";
}

function withBlocks(doc: JSONContent, blocks: JSONContent[]): JSONContent {
  return { ...doc, content: blocks.length > 0 ? blocks : [{ type: "paragraph" }] };
}

/** Remove the leading `/word ` from the first paragraph; drop that paragraph if nothing is left and more follows. */
function stripLeadingSlashCommand(doc: JSONContent): JSONContent {
  const head = doc.content?.[0];
  const match = head?.type === "paragraph" ? LEADING_COMMAND_PATTERN.exec(plainText(head)) : null;
  if (!match) return doc;
  const stripped = dropLeadingDocText(doc, match[0].length);
  const [first, ...rest] = stripped.content ?? [];
  return rest.length > 0 && isEmptyParagraph(first) ? withBlocks(stripped, rest) : stripped;
}

function trimTrailingEmptyParagraphs(blocks: JSONContent[]): JSONContent[] {
  let end = blocks.length;
  while (end > 1 && isEmptyParagraph(blocks[end - 1])) end -= 1;
  return blocks.slice(0, end);
}

function shrugSeparator(text: string): string {
  return text === "" || /\s$/.test(text) ? "" : " ";
}

/** Append `¯\_(ツ)_/¯` to the last paragraph (space-separated), or as a new paragraph after a non-paragraph block. */
function appendShrug(doc: JSONContent): JSONContent {
  const blocks = trimTrailingEmptyParagraphs(doc.content ?? []);
  const last = blocks.at(-1);
  if (last?.type !== "paragraph") {
    return withBlocks(doc, [...blocks, { type: "paragraph", content: [{ type: "text", text: SHRUG }] }]);
  }
  const text = `${shrugSeparator(plainText(last))}${SHRUG}`;
  const appended = { ...last, content: [...(last.content ?? []), { type: "text", text }] };
  return withBlocks(doc, [...blocks.slice(0, -1), appended]);
}

/** Put the exact XEP-0245 `/me ` prefix at the start of the first paragraph. */
function prependMeCommand(doc: JSONContent): JSONContent {
  const [first, ...rest] = doc.content ?? [];
  const prefix: JSONContent = { type: "text", text: ME_COMMAND_PREFIX };
  if (first?.type !== "paragraph") {
    return withBlocks(doc, [{ type: "paragraph", content: [prefix] }, ...(first ? [first] : []), ...rest]);
  }
  return withBlocks(doc, [{ ...first, content: [prefix, ...(first.content ?? [])] }, ...rest]);
}

/**
 * The document a sending built-in actually sends, derived from the composer
 * document that still starts with the typed `/command`. Formatting on the
 * remaining text survives. `/me` is canonicalized to the exact lowercase
 * `/me ` prefix so XEP-0245 receivers match it (e.g. `/ME  waves` → `/me waves`).
 */
export function rewriteBuiltinSendDoc(rewrite: BuiltinSendRewrite, doc: JSONContent): JSONContent {
  const stripped = stripLeadingSlashCommand(doc);
  return rewrite === "shrug" ? appendShrug(stripped) : prependMeCommand(stripped);
}
