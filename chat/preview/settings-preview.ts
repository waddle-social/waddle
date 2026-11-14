import { connectionStore } from "@/lib/connection-store";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * Preview fixtures for the settings screens (`?screen=settings…`):
 *   settings            connected, profile loaded
 *   settings-offline    no XMPP client (Reconnect banner, controls disabled)
 *   settings-publishing connected; publishVCard4 never settles (click Save)
 * Published / tune-error states are reached by clicking in the shot spec.
 */
export function seedSettings(c: ChatAppController, screen: string): void {
  if (!screen.startsWith("settings")) return;
  // Route sync settles the /_preview URL onto home after seeding; land after it.
  c.ui.activePage.value = "settings";
  setTimeout(() => { c.ui.activePage.value = "settings"; }, 0);
  c.version.serverVersion.value = { name: "waddle", version: "0.9.0-rc.1" };
  if (screen === "settings-offline") return;
  const never = new Promise<never>(() => {});
  const methods: Record<string, (...args: unknown[]) => unknown> = {
    fetchVCard4: async () => ({
      fullName: "Ada Lovelace",
      nickname: "Ada",
      pronouns: "she/her",
      url: "https://ada.example.org",
      note: "Android client, chat history and too much coffee.",
    }),
    publishVCard4: () => (screen === "settings-publishing" ? never : Promise.resolve()),
  };
  // Identity getters answer; every other call stays pending forever so no
  // fixture data is overwritten. `then` stays undefined so the fake is never
  // mistaken for a promise.
  const identity: Record<string, string> = { bareJid: "ada@waddle.test", fullJid: "ada@waddle.test/preview", xmppResource: "preview" };
  const fake = new Proxy(methods, {
    get: (target, prop) => {
      if (typeof prop === "symbol" || prop === "then" || prop === "catchup" || prop === "agent") return undefined;
      if (prop.startsWith("list")) return () => [];
      return identity[prop] ?? target[prop] ?? (() => never);
    },
  });
  connectionStore.client = fake as unknown as BrowserXmppClient;
}
