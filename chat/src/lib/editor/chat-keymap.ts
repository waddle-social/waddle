import { extensions, type CommandProps, type Editor, type JSONContent } from "@tiptap/core";
import { Plugin, PluginKey, Selection } from "@tiptap/pm/state";

const { Keymap } = extensions;

interface ChatKeymapOptions {
  onSubmit: (doc: JSONContent) => boolean | void;
}

export const chatKeymapPluginKey = new PluginKey("chatKeymap");

export const ChatKeymap = Keymap.extend<ChatKeymapOptions>({
  priority: 99,

  addOptions() {
    return {
      ...this.parent?.(),
      onSubmit: () => false,
    };
  },

  addKeyboardShortcuts() {
    const shortcutsWithoutEnter = { ...(this.parent?.() ?? {}) };
    delete shortcutsWithoutEnter.Enter;
    return shortcutsWithoutEnter;
  },

  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: chatKeymapPluginKey,
        props: {
          handleKeyDown: (view, event) => {
            if (!isPlainEnter(event)) return false;
            if (event.isComposing || isLegacyCompositionKey(event) || view.composing) return false;
            if (!this.editor.isEditable) return false;

            if (this.editor.isEmpty) return true;

            return this.editor.commands.first(({ commands }) => [
              () => commands.command(exitCodeBlockFromBlankLine),
              () => commands.newlineInCode(),
              () => commands.createParagraphNear(),
              () => commands.liftEmptyBlock(),
              () => submitAtChatBoundary(this.editor, this.options.onSubmit),
              () => commands.splitBlock(),
            ]);
          },
        },
      }),
    ];
  },
});

export function isChatSubmitBoundary(editor: Editor): boolean {
  const { selection } = editor.state;
  const { $to } = selection;

  if ($to.parent.isTextblock) {
    if ($to.parentOffset !== $to.parent.content.size) return false;
    if ($to.depth !== 1 || $to.parent.type.name !== "paragraph") return false;
  } else if ($to.depth > 0) {
    return false;
  }

  return hasOnlyEmptyTopLevelParagraphsAfter(editor);
}

function submitAtChatBoundary(editor: Editor, onSubmit: ChatKeymapOptions["onSubmit"]): boolean {
  if (!isChatSubmitBoundary(editor)) return false;
  return onSubmit(editor.getJSON()) !== false;
}

function exitCodeBlockFromBlankLine({ state, tr }: CommandProps): boolean {
  const { selection } = state;
  const { $from, empty } = selection;
  const codeBlockType = state.schema.nodes.codeBlock;
  const paragraphType = state.schema.nodes.paragraph;

  if (!empty || !codeBlockType || !paragraphType || $from.parent.type !== codeBlockType) return false;
  if ($from.parentOffset !== $from.parent.content.size) return false;
  if (!$from.parent.textContent.endsWith("\n")) return false;

  const codeParent = $from.node(-1);
  const insertIndex = $from.indexAfter(-1);
  if (!codeParent.canReplaceWith(insertIndex, insertIndex, paragraphType)) return false;

  const insertPosition = $from.after();
  const paragraph = paragraphType.createAndFill();
  if (!paragraph) return false;
  if (!tr.doc.eq(state.doc)) return false;

  tr.delete($from.pos - 1, $from.pos);
  const mappedInsertPosition = tr.mapping.map(insertPosition);
  tr.replaceWith(mappedInsertPosition, mappedInsertPosition, paragraph);
  tr.setSelection(Selection.near(tr.doc.resolve(mappedInsertPosition), 1));
  tr.scrollIntoView();

  return true;
}

function isPlainEnter(event: KeyboardEvent): boolean {
  return event.key === "Enter" && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey;
}

function isLegacyCompositionKey(event: KeyboardEvent): boolean {
  return Reflect.get(event, "keyCode") === 229;
}

function hasOnlyEmptyTopLevelParagraphsAfter(editor: Editor): boolean {
  const { doc, selection } = editor.state;
  const { $to } = selection;
  const nextTopLevelIndex = $to.depth === 0 ? $to.indexAfter(0) : $to.indexAfter(0);

  for (let index = nextTopLevelIndex; index < doc.childCount; index += 1) {
    const node = doc.child(index);
    if (node.type.name !== "paragraph" || node.content.size > 0) return false;
  }

  return true;
}
