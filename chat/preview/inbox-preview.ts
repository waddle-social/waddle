import type { ChatAppController } from "@/shell/chat-app-controller";

/** Preview fixtures for the inbox screens (`?screen=inbox-…`). */
export function seedInbox(c: ChatAppController, screen: string): void {
  if (!screen.startsWith("inbox")) return;
  void c;
}
