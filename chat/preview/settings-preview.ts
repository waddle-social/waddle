import type { ChatAppController } from "@/shell/chat-app-controller";

/** Preview fixtures for the settings screens (`?screen=settings-…`). */
export function seedSettings(c: ChatAppController, screen: string): void {
  if (!screen.startsWith("settings")) return;
  void c;
}
