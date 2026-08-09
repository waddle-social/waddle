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
    const activeConversation = readFileSync(new URL("../src/shell/controllers/use-active-conversation.ts", import.meta.url), "utf8");
    const sendOrchestration = readFileSync(new URL("../src/shell/controllers/use-send-orchestration.ts", import.meta.url), "utf8");
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");

    expect(activeConversation).toContain("dmMessaging.loadErrorPeerJid.value === peer.peerJid");
    expect(activeConversation).toContain("activeActionError.value === dmMessaging.loadErrorMessage.value");
    expect(activeConversation).toContain('? "Try again"');
    // The retry affordance is an explicit user action: it first clears a
    // superseded-session latch (own-resume conflict displaced this tab)
    // and then reloads the active DM conversation.
    expect(sendOrchestration).toContain("await xmppClient.value?.recoverSupersededSession()");
    expect(sendOrchestration).toContain("activeDmPeer.value?.peerJid !== peerJid");
    expect(sendOrchestration).toContain("await dmMessaging.loadMessages(peerJid)");
    expect(readyShell).toContain('@retry-load="retryActiveLoad"');
  });
});
