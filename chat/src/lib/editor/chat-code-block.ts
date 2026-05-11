import type { JSONContent } from "@tiptap/core";
import { CodeBlock } from "@tiptap/extension-code-block";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";
import type { EditorState } from "@tiptap/pm/state";

export const ChatCodeBlock = CodeBlock.extend({
  addCommands() {
    const parent = this.parent?.();

    return {
      ...parent,
      toggleCodeBlock:
        (attributes) =>
        ({ editor, state, commands }) => {
          const { empty } = state.selection;

          if (empty || editor.isActive(this.name, attributes)) {
            return commands.toggleNode(this.name, "paragraph", attributes);
          }

          const selection = selectedTopLevelTextblockSelection(state);
          if (!selection) {
            return commands.toggleNode(this.name, "paragraph", attributes);
          }

          return commands.insertContentAt({ from: selection.from, to: selection.to }, {
            type: this.name,
            attrs: { language: attributes?.language ?? null },
            ...codeBlockContent(selection.text),
          });
        },
    };
  },
});

type SelectedTextblock = {
  from: number;
  to: number;
  text: string;
};

function selectedTopLevelTextblockSelection(state: EditorState): { from: number; to: number; text: string } | null {
  const { from, to } = state.selection;
  const blocks: SelectedTextblock[] = [];
  let unsupportedSelection = false;

  state.doc.nodesBetween(from, to, (node, pos, parent) => {
    if (unsupportedSelection) return false;
    if (parent !== state.doc) {
      if (node.isTextblock) unsupportedSelection = true;
      return true;
    }
    if (!node.isTextblock) {
      unsupportedSelection = true;
      return false;
    }
    if (!coversWholeTextblock({ from, to }, node, pos)) {
      unsupportedSelection = true;
      return false;
    }
    blocks.push({
      from: pos,
      to: pos + node.nodeSize,
      text: textblockText(node),
    });
    return false;
  });

  if (unsupportedSelection || blocks.length === 0) return null;
  if (blocks.length === 1 && !blocks[0].text.includes("\n")) return null;
  return {
    from: blocks[0].from,
    to: blocks[blocks.length - 1].to,
    text: blocks.map((block) => block.text).join("\n"),
  };
}

function coversWholeTextblock(
  selection: { from: number; to: number },
  node: ProseMirrorNode,
  pos: number,
): boolean {
  const contentFrom = pos + 1;
  const contentTo = contentFrom + node.content.size;
  return selection.from <= contentFrom && selection.to >= contentTo;
}

function textblockText(node: ProseMirrorNode): string {
  return node.textBetween(0, node.content.size, "\n", "\n");
}

function codeBlockContent(text: string): Pick<JSONContent, "content"> {
  return text ? { content: [{ type: "text", text }] } : {};
}
