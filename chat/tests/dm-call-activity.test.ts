import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  $dmCallActivities,
  applyDmCallEvent,
  clearDmCallActivities,
  clearDmCallActivity,
  pruneExpiredDmCallActivities,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { CallEvent } from "../src/lib/calls/types";

const self = "alice@waddle.test";
const bob = "bob@waddle.test";
const audio = { audio: true, video: false };
const now = new Date("2026-05-25T10:00:00.000Z");

describe("DM call activity", () => {
  beforeEach(() => {
    clearDmCallActivities();
  });

  afterEach(() => {
    clearDmCallActivities();
  });

  test("tracks an unresolved remote proposal as ringing activity", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-1",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)).toEqual({
      peerJid: bob,
      sid: "call-1",
      media: audio,
      state: "ringing",
      direction: "incoming",
      updatedAt: now.toISOString(),
    });
  });

  test("uses the to JID for self-sent carbon events from another resource", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/phone`,
        to: `${bob}/desktop`,
        sid: "call-2",
        media: { audio: true, video: true },
      },
      selfBareJid: self,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)?.direction).toBe("outgoing");
    expect(readDmCallActivity(bob, now)?.media.video).toBe(true);
  });

  test("marks a call accepted when a peer proceeds on one resource", () => {
    const propose: CallEvent = {
      kind: "propose",
      from: `${self}/web`,
      sid: "call-3",
      media: audio,
    };
    applyDmCallEvent({
      event: propose,
      selfBareJid: self,
      to: bob,
      timestamp: now.toISOString(),
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-3",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:00:10.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      peerJid: bob,
      sid: "call-3",
      media: audio,
      state: "accepted",
      direction: "outgoing",
    });
  });

  test("removes activity when finish arrives for the same sid", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-4",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${bob}/phone`,
        sid: "call-4",
        reason: "success",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("self-sent terminal events can clear by sid when the peer hint is gone", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/web`,
        to: `${bob}/phone`,
        sid: "call-6",
        media: audio,
      },
      selfBareJid: self,
      timestamp: now.toISOString(),
      now,
    });
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-6",
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:00:10.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${self}/other`,
        sid: "call-6",
        reason: "success",
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("older MAM call events cannot regress a newer activity state", () => {
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "call-7",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-7",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toMatchObject({
      sid: "call-7",
      state: "accepted",
      updatedAt: "2026-05-25T10:05:00.000Z",
    });
  });

  test("terminal MAM events prevent older proposals for the same sid from resurrecting activity", () => {
    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${bob}/phone`,
        sid: "call-8",
        reason: "success",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "call-8",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("self-sent terminal events with a to peer tombstone older sibling proposals", () => {
    applyDmCallEvent({
      event: {
        kind: "finish",
        from: `${self}/phone`,
        to: `${bob}/desktop`,
        sid: "call-9",
        reason: "success",
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:05:00.000Z",
      now,
    });

    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/phone`,
        to: `${bob}/desktop`,
        sid: "call-9",
        media: audio,
      },
      selfBareJid: self,
      timestamp: "2026-05-25T10:00:00.000Z",
      now,
    });

    expect(readDmCallActivity(bob, now)).toBeNull();
  });

  test("does not let 24h-old catch-up proposals resurrect old calls", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "old-call",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-24T09:59:59.000Z",
      now,
    });

    expect($dmCallActivities.get()).toEqual({});
  });

  test("does not let 24h-old accepted catch-up events resurrect old calls", () => {
    applyDmCallEvent({
      event: {
        kind: "proceed",
        from: `${bob}/phone`,
        sid: "old-call",
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: "2026-05-24T09:59:59.000Z",
      now,
    });

    expect($dmCallActivities.get()).toEqual({});
  });

  test("prunes visible unresolved activity after the 24h XEP-0353 fallback window", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${bob}/phone`,
        sid: "aging-call",
        media: audio,
      },
      selfBareJid: self,
      to: `${self}/web`,
      timestamp: now.toISOString(),
      now,
    });

    expect(readDmCallActivity(bob, now)?.sid).toBe("aging-call");

    pruneExpiredDmCallActivities(new Date("2026-05-26T10:00:01.000Z"));

    expect(readDmCallActivity(bob, new Date("2026-05-26T10:00:01.000Z"))).toBeNull();
  });

  test("local cleanup can clear an optimistic outgoing proposal", () => {
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: `${self}/web`,
        sid: "call-5",
        media: audio,
      },
      selfBareJid: self,
      to: bob,
      timestamp: now.toISOString(),
      now,
    });

    clearDmCallActivity(bob, "call-5");

    expect(readDmCallActivity(bob, now)).toBeNull();
  });
});
