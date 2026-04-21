import StarterKit from "@tiptap/starter-kit";
import Placeholder from "@tiptap/extension-placeholder";
import type { Extensions } from "@tiptap/vue-3";
import { ChatLink } from "@/lib/editor/chat-link";

interface ChatEditorExtensionsOptions {
  placeholder?: string | (() => string);
  includePlaceholder?: boolean;
}

export function createChatEditorExtensions(options: ChatEditorExtensionsOptions = {}): Extensions {
  const extensions: Extensions = [
    StarterKit.configure({
      heading: false,
      horizontalRule: false,
      link: false,
      underline: false,
    }),
    ChatLink.configure({
      openOnClick: false,
      autolink: true,
      HTMLAttributes: {
        class: "text-primary underline decoration-primary/40 hover:decoration-primary transition-colors",
      },
    }),
  ];

  if (options.includePlaceholder ?? true) {
    extensions.push(
      Placeholder.configure({
        placeholder: options.placeholder ?? "",
      }),
    );
  }

  return extensions;
}
