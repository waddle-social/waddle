import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("DM threading UI contract", () => {
  test("exposes DM thread list and thread panel affordances", () => {
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");
    const dmPanel = readFileSync(new URL("../src/components/chat/DmPanel.vue", import.meta.url), "utf8");
    const controller = readFileSync(new URL("../src/shell/chat-app-controller.ts", import.meta.url), "utf8");

    expect(readyShell).toContain(":thread-entries=\"activeDmThreadEntries\"");
    expect(readyShell).toContain("@select-thread=\"openThread\"");
    expect(readyShell).toContain("threadPanelConversationActive");
    expect(readyShell).toContain("threadPanelIsDm");
    expect(controller).toContain('if (panel === "thread") return activeThreadStack.value.length > 0');
    expect(controller).not.toContain('if (ui.sidebarMode.value !== "channels" || ui.activePage.value !== "chat") return false;');
    expect(dmPanel).toContain("threadEntries?: MessageThreadEntry[]");
    expect(dmPanel).toContain("selectThread: [threadId: string]");
  });
});
