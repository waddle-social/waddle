import { describe, expect, test } from "bun:test";
import type { CallActivityDockEntry } from "../src/lib/calls/call-activity-dock";
import type { CallState } from "../src/lib/calls/types";
import {
  callEntryDescription,
  callEntryDetail,
  callEntryEyebrow,
  callEntryKindLabel,
  callEntryLabel,
  callEntryParticipantInitial,
  callEntryParticipantPreview,
  callEntryStatus,
  callEntryToneClass,
  callEntryVisibleParticipantLabels,
  callEntryVisualTone,
  endCallEntryButtonText,
  endCallEntryLabel,
  isSameCallEntry,
} from "../src/components/chat/home-call-entry-presentation";

const idle: CallState = { phase: "idle" };

function channelEntry(overrides: Partial<Extract<CallActivityDockEntry, { kind: "channel" }>> = {}): CallActivityDockEntry {
  return {
    kind: "channel",
    key: "channel:general@muc.example.com",
    channelId: "general",
    roomJid: "general@muc.example.com",
    title: "General",
    participantCount: 2,
    participantLabels: ["alice", "bob"],
    media: { audio: true, video: false },
    isKnownChannel: true,
    isActive: false,
    ...overrides,
  };
}

function dmEntry(overrides: Partial<Extract<CallActivityDockEntry, { kind: "dm" }>> = {}): CallActivityDockEntry {
  return {
    kind: "dm",
    key: "dm:bob@example.com:sid-1",
    peerJid: "bob@example.com",
    sid: "sid-1",
    title: "Bob",
    media: { audio: true, video: true },
    state: "ringing",
    direction: "incoming",
    updatedAt: "",
    isActive: false,
    ...overrides,
  };
}

describe("channel entries", () => {
  test("status, kind, and participant helpers", () => {
    const entry = channelEntry();
    expect(callEntryStatus(entry)).toBe("2 people");
    expect(callEntryKindLabel(entry)).toBe("Group call");
    expect(callEntryKindLabel(channelEntry({ media: { audio: true, video: true } }))).toBe("Group video call");
    expect(callEntryKindLabel(channelEntry({ isKnownChannel: false }))).toBe("Group call syncing");
    expect(callEntryParticipantPreview(entry)).toContain("alice");
    expect(callEntryVisibleParticipantLabels(channelEntry({ participantLabels: ["a", "b", "c", "d"] }))).toEqual(["a", "b", "c"]);
    expect(callEntryParticipantInitial("alice")).toBe("A");
    expect(callEntryParticipantInitial("  ")).toBe("?");
  });

  test("description names the channel context and participants", () => {
    expect(callEntryDescription(channelEntry(), idle, null))
      .toBe("2 people connected in this channel: alice, bob.");
    expect(callEntryDescription(channelEntry({ media: { audio: true, video: true } }), idle, null))
      .toBe("2 people connected to the video call in this channel: alice, bob.");
  });

  test("eyebrow reflects presence in the call", () => {
    expect(callEntryEyebrow(channelEntry(), idle, null)).toBe("Live now");
    expect(callEntryEyebrow(channelEntry({ isActive: true }), idle, null)).toBe("You're here");
  });

  test("end affordance copy", () => {
    expect(endCallEntryLabel(channelEntry())).toBe("Leave General call");
    expect(endCallEntryButtonText(channelEntry())).toBe("Leave call");
  });
});

describe("dm entries", () => {
  test("ringing direction drives status, eyebrow, and tone", () => {
    const incoming = dmEntry();
    expect(callEntryStatus(incoming)).toBe("Ringing");
    expect(callEntryEyebrow(incoming, idle, null)).toBe("Incoming call");
    expect(callEntryVisualTone(incoming, idle, null)).toBe("warning");

    const outgoing = dmEntry({ direction: "outgoing" });
    expect(callEntryStatus(outgoing)).toBe("Calling");
    expect(callEntryVisualTone(outgoing, idle, null)).toBe("primary");
  });

  test("accepted call without local resume details reads as syncing", () => {
    const accepted = dmEntry({ state: "accepted", direction: "unknown" });
    expect(callEntryEyebrow(accepted, idle, null)).toBe("Syncing");
    expect(callEntryDescription(accepted, idle, null)).toBe("Reconnect details are not available on this tab yet.");
    expect(callEntryVisualTone(accepted, idle, null)).toBe("primary");
    expect(callEntryDetail(accepted, idle, null)).toBe("Video call · Details pending");
  });

  test("accepted call owned by another resource reads as other-device", () => {
    const otherDevice = dmEntry({
      state: "accepted",
      direction: "unknown",
      remoteFullJid: "bob@example.com/phone",
      join: { url: "wss://sfu", room: "r", identity: "me@example.com/other", token: "t" },
    });
    const self = "me@example.com/this";
    expect(callEntryEyebrow(otherDevice, idle, self)).toBe("Other device");
    expect(callEntryDescription(otherDevice, idle, self)).toBe("This call is live on another browser or device.");
    expect(callEntryVisualTone(otherDevice, idle, self)).toBe("warning");
    expect(callEntryDetail(otherDevice, idle, self)).toBe("Video call · Other device");
  });

  test("unknown media degrades copy without inventing a medium", () => {
    const unknown = dmEntry({ mediaKnown: false });
    expect(callEntryKindLabel(unknown)).toBe("Call");
    expect(callEntryDescription(unknown, idle, null)).toBe("Incoming call details are still syncing.");
    expect(endCallEntryLabel(unknown)).toBe("End Bob call");
    expect(endCallEntryButtonText(unknown)).toBe("End call");
  });

  test("aria label composes action, kind, status, and description", () => {
    const label = callEntryLabel(dmEntry(), idle, null);
    expect(label).toContain("Bob");
    expect(label).toContain("Video call");
    expect(label).toContain("Ringing");
  });
});

describe("isSameCallEntry", () => {
  test("channels match by normalized room, dms by peer + sid", () => {
    expect(isSameCallEntry(channelEntry())(channelEntry({ key: "other" }))).toBe(true);
    expect(isSameCallEntry(channelEntry())(channelEntry({ roomJid: "other@muc.example.com" }))).toBe(false);
    expect(isSameCallEntry(dmEntry())(dmEntry({ peerJid: "BOB@example.com" }))).toBe(true);
    expect(isSameCallEntry(dmEntry())(dmEntry({ sid: "sid-2" }))).toBe(false);
    expect(isSameCallEntry(dmEntry())(channelEntry())).toBe(false);
  });
});

describe("tone classes", () => {
  test("each tone maps to its palette", () => {
    expect(callEntryToneClass("warning")).toContain("border-warning/25");
    expect(callEntryToneClass("primary")).toContain("border-primary/25");
    expect(callEntryToneClass("success")).toContain("border-success/20");
  });
});
