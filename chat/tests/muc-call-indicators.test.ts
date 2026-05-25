import { describe, expect, test } from "bun:test";
import {
  callParticipantCountForChannel,
  collapsedGroupBadgeModel,
  normalizeMucServiceDomain,
} from "../src/lib/calls/muc-call-indicators";

describe("callParticipantCountForChannel", () => {
  test("matches channel jid with normalized room jid counts", () => {
    expect(callParticipantCountForChannel(
      { id: "general", jid: "General@MUC.Test/mobile" },
      { "general@muc.test": 2 },
      new Set(),
    )).toBe(2);
  });

  test("matches id-only channel rows through the managed MUC domain", () => {
    expect(callParticipantCountForChannel(
      { id: "general" },
      { "general@muc.test": 1 },
      new Set(["General@MUC.Test"]),
      "muc.test",
    )).toBe(1);
  });

  test("does not guess a channel room from localpart without a trusted domain", () => {
    expect(callParticipantCountForChannel(
      { id: "general" },
      { "general@muc.test": 1 },
      new Set(["General@MUC.Test"]),
    )).toBe(0);
  });

  test("does not let an unrelated active call hide this channel's count", () => {
    expect(callParticipantCountForChannel(
      { id: "general", jid: "general@muc.test" },
      {
        "general@muc.test": 1,
        "random@muc.test": 4,
      },
      new Set(["random@muc.test"]),
    )).toBe(1);
  });

  test("fallback active-room matching does not use substring collisions", () => {
    expect(callParticipantCountForChannel(
      { id: "gen" },
      { "general@muc.test": 3 },
      new Set(["general@muc.test"]),
    )).toBe(0);
  });

  test("does not match same-localpart calls from another MUC domain", () => {
    expect(callParticipantCountForChannel(
      { id: "general", jid: "general@muc.test" },
      { "general@other-muc.test": 3 },
      new Set(["general@other-muc.test"]),
    )).toBe(0);
  });
});

describe("normalizeMucServiceDomain", () => {
  test("accepts discovered MUC service JIDs and bare domains", () => {
    expect(normalizeMucServiceDomain("custom-muc.example.test")).toBe("custom-muc.example.test");
    expect(normalizeMucServiceDomain("rooms@example.test/resource")).toBe("example.test");
  });
});

describe("collapsedGroupBadgeModel", () => {
  test("renders call count alongside mentions instead of hiding notifications", () => {
    expect(collapsedGroupBadgeModel({
      callCount: 2,
      mentions: 1,
      unread: 5,
      hasActivity: true,
    })).toEqual({
      callCount: 2,
      notification: { kind: "mentions", count: 1 },
    });
  });

  test("preserves unread and activity badges when no mention is present", () => {
    expect(collapsedGroupBadgeModel({
      callCount: 1,
      mentions: 0,
      unread: 3,
      hasActivity: true,
    })).toEqual({
      callCount: 1,
      notification: { kind: "unread", count: 3 },
    });
    expect(collapsedGroupBadgeModel({
      callCount: 0,
      mentions: 0,
      unread: 0,
      hasActivity: true,
    })).toEqual({
      callCount: 0,
      notification: { kind: "activity" },
    });
  });
});
