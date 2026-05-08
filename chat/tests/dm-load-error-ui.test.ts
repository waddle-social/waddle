import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("DM load error UI contract", () => {
  test("renders an announced retryable error state", () => {
    const contentArea = readFileSync(new URL("../src/components/chat/ContentArea.vue", import.meta.url), "utf8");

    expect(contentArea).toContain("errorActionLabel?: string | null");
    expect(contentArea).toContain("retryLoad: []");
    expect(contentArea).toContain('role="alert"');
    expect(contentArea).toContain('aria-live="assertive"');
    expect(contentArea).toContain("@click=\"emit('retryLoad')\"");
    expect(contentArea).toContain("Messages are not available right now.");
  });

  test("wires the retry action only for active DM load failures", () => {
    const chatController = readFileSync(new URL("../src/shell/chat-app-controller.ts", import.meta.url), "utf8");
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");

    expect(chatController).toContain("dmMessaging.loadErrorPeerJid.value === peer.peerJid");
    expect(chatController).toContain("activeActionError.value === dmMessaging.loadErrorMessage.value");
    expect(chatController).toContain('? "Try again"');
    expect(chatController).toContain("void dmMessaging.loadMessages(peer.peerJid)");
    expect(readyShell).toContain('@retry-load="retryActiveLoad"');
  });
});
