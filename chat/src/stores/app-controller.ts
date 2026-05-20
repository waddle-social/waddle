import { shallowRef, type ShallowRef } from "vue";
import type { ChatAppController } from "@/shell/chat-app-controller";

// Module-level reactive handle to the chat controller. AppShell instantiates
// the controller once (inside a Vue setup so onMounted/onUnmounted hooks
// work) and writes it here; Vue islands mounted in AppShell's slot read it
// back to share connection / channel / unread state without prop drilling.
//
// `shallowRef` is correct here: the controller object itself is the
// reactivity unit, not its (deeply nested, already-reactive) inner refs.
// Writing a fresh controller triggers consumers; mutating its inner refs
// is tracked by Vue's normal reactivity through those refs themselves.
export const appController: ShallowRef<ChatAppController | null> = shallowRef(null);
