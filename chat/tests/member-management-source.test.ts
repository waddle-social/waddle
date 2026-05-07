import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("member management source contract", () => {
  test("presence-inferred occupants are displayed but not editable as affiliations", () => {
    const chatApp = readFileSync(new URL("../src/components/ChatApp.vue", import.meta.url), "utf8");
    const memberManagement = readFileSync(new URL("../src/components/modals/MemberManagement.vue", import.meta.url), "utf8");

    expect(chatApp).toContain("authoritativeMemberJids");
    expect(chatApp).toContain("inferredMemberJids");
    expect(chatApp).toContain(':inferred-member-jids="inferredMemberJids"');
    expect(memberManagement).toContain("inferredMemberJids: Set<string>");
    expect(memberManagement).toContain('!inferredMemberJids.has(member.jid)');
    expect(memberManagement).toContain("Synced from presence");
  });
});
