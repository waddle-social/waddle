import { describe, it, expect } from "bun:test";
import type { CreateChannelResult, CreateSpaceResult, CreateMucResult } from "@/lib/chat-ui";

// ── Helpers ──────────────────────────────────────────────────────────

function hasRoom(result: CreateChannelResult): result is CreateMucResult {
  return result.intent !== "space";
}

/** Simulate the ChatApp post-create routing decision. */
function routeAfterCreate(result: CreateChannelResult): {
  closeDialog: boolean;
  loadMessages: boolean;
  channelId: string | null;
} {
  if (result.intent === "space") {
    return { closeDialog: true, loadMessages: false, channelId: null };
  }
  return { closeDialog: true, loadMessages: true, channelId: result.channelId };
}

// ── Type narrowing ────────────────────────────────────────────────────

describe("CreateChannelResult type narrowing", () => {
  it("space intent narrows to CreateSpaceResult with no channelId", () => {
    const result: CreateChannelResult = {
      intent: "space",
      spaceId: "eng",
      spaceName: "Engineering",
    };
    if (result.intent === "space") {
      const typed: CreateSpaceResult = result;
      expect(typed.spaceId).toBe("eng");
      expect(typed.spaceName).toBe("Engineering");
      // @ts-expect-error channelId does not exist on CreateSpaceResult
      void typed.channelId;
    } else {
      throw new Error("Expected space intent");
    }
  });

  it("muc intent narrows to CreateMucResult with channelId", () => {
    const result: CreateChannelResult = {
      intent: "muc",
      channelId: "general",
      channelJid: "general@muc.example.org",
      channelName: "General",
      channelType: "text",
    };
    if (result.intent === "muc") {
      const typed: CreateMucResult = result;
      expect(typed.channelId).toBe("general");
    } else {
      throw new Error("Expected muc intent");
    }
  });

  it("space-muc intent carries both channelId and spaceNode", () => {
    const result: CreateChannelResult = {
      intent: "space-muc",
      channelId: "announcements",
      channelJid: "announcements@muc.example.org",
      channelName: "Announcements",
      channelType: "text",
      spaceNode: "eng",
    };
    if (result.intent === "space-muc") {
      expect(result.channelId).toBe("announcements");
      expect(result.spaceNode).toBe("eng");
    } else {
      throw new Error("Expected space-muc intent");
    }
  });

  it("space-with-muc intent carries channelId and spaceNode", () => {
    const result: CreateChannelResult = {
      intent: "space-with-muc",
      channelId: "general",
      channelJid: "general@muc.example.org",
      channelName: "General",
      channelType: "text",
      spaceNode: "eng",
    };
    if (result.intent === "space-with-muc") {
      expect(result.channelId).toBe("general");
      expect(result.spaceNode).toBe("eng");
    } else {
      throw new Error("Expected space-with-muc intent");
    }
  });
});

// ── Post-create routing branches ─────────────────────────────────────

describe("post-create routing", () => {
  it("space-only: closes dialog, does not load messages, leaves channel null", () => {
    const result: CreateChannelResult = {
      intent: "space",
      spaceId: "eng",
      spaceName: "Engineering",
    };
    const route = routeAfterCreate(result);
    expect(route.closeDialog).toBe(true);
    expect(route.loadMessages).toBe(false);
    expect(route.channelId).toBeNull();
  });

  it("standalone muc: closes dialog and loads messages for created channel", () => {
    const result: CreateChannelResult = {
      intent: "muc",
      channelId: "general",
      channelJid: "general@muc.example.org",
      channelName: "General",
      channelType: "text",
    };
    const route = routeAfterCreate(result);
    expect(route.closeDialog).toBe(true);
    expect(route.loadMessages).toBe(true);
    expect(route.channelId).toBe("general");
  });

  it("muc-in-existing-space: closes dialog and loads messages for created channel", () => {
    const result: CreateChannelResult = {
      intent: "space-muc",
      channelId: "announcements",
      channelJid: "announcements@muc.example.org",
      channelName: "Announcements",
      channelType: "text",
      spaceNode: "eng",
    };
    const route = routeAfterCreate(result);
    expect(route.closeDialog).toBe(true);
    expect(route.loadMessages).toBe(true);
    expect(route.channelId).toBe("announcements");
  });

  it("space-with-muc: closes dialog and loads messages for created channel", () => {
    const result: CreateChannelResult = {
      intent: "space-with-muc",
      channelId: "general",
      channelJid: "general@muc.example.org",
      channelName: "General",
      channelType: "text",
      spaceNode: "eng",
    };
    const route = routeAfterCreate(result);
    expect(route.closeDialog).toBe(true);
    expect(route.loadMessages).toBe(true);
    expect(route.channelId).toBe("general");
  });

  it("hasRoom guard returns false for space-only and true for all muc variants", () => {
    const spaceResult: CreateChannelResult = { intent: "space", spaceId: "x", spaceName: "X" };
    const mucResult: CreateChannelResult = { intent: "muc", channelId: "c", channelJid: "c@m", channelName: "C", channelType: "text" };
    const spaceMucResult: CreateChannelResult = { intent: "space-muc", channelId: "c", channelJid: "c@m", channelName: "C", channelType: "text", spaceNode: "s" };
    const spaceWithMucResult: CreateChannelResult = { intent: "space-with-muc", channelId: "c", channelJid: "c@m", channelName: "C", channelType: "text", spaceNode: "s" };

    expect(hasRoom(spaceResult)).toBe(false);
    expect(hasRoom(mucResult)).toBe(true);
    expect(hasRoom(spaceMucResult)).toBe(true);
    expect(hasRoom(spaceWithMucResult)).toBe(true);
  });
});
