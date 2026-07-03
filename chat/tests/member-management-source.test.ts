import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("member management source contract", () => {
  test("presence-inferred occupants are displayed but not editable as affiliations", () => {
    const chatController = readFileSync(new URL("../src/shell/controllers/use-member-directory.ts", import.meta.url), "utf8");
    const chatModals = readFileSync(new URL("../src/components/chat/ChatAppModals.vue", import.meta.url), "utf8");
    const memberManagement = readFileSync(new URL("../src/components/modals/MemberManagement.vue", import.meta.url), "utf8");

    expect(chatController).toContain("authoritativeMemberJids");
    expect(chatController).toContain("inferredMemberJids");
    expect(chatModals).toContain(':inferred-member-jids="inferredMemberJids"');
    expect(memberManagement).toContain("inferredMemberJids: Set<string>");
    expect(memberManagement).toContain('!inferredMemberJids.has(member.jid)');
    expect(memberManagement).toContain("Synced from presence");
  });
});
