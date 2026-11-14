import type { ChatAppController } from "@/shell/chat-app-controller";

/** Preview fixtures for the admin screens (`?screen=admin-…`). */
export function seedAdmin(c: ChatAppController, screen: string): void {
  if (!screen.startsWith("admin")) return;
  void c;
}
