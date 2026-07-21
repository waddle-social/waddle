import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  clearCallState,
} from "../src/lib/calls/call-store";
import {
  answerIncomingDmCallActivity,
  endRecoveredDmCallAction,
  resumeDmCallActivity,
  startDmCallAction,
} from "../src/lib/calls/dm-call-actions";
import {
  applyDmCallEvent,
  clearDmCallActivities,
  clearDmCallActivity,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { CallWireSender } from "../src/lib/calls/outbound";
import { __resetCallLifecycleTelemetryForTesting } from "../src/lib/calls/call-lifecycle-telemetry";
import { __setFaroForTesting } from "../src/lib/telemetry";

function jwtWithExp(exp: number): string {
  return [
    base64Url(JSON.stringify({ alg: "none", typ: "JWT" })),
    base64Url(JSON.stringify({ exp })),
    "sig",
  ].join(".");
}

function base64Url(value: string): string {
  return btoa(value).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

afterEach(() => {
  clearCallState();
  clearDmCallActivities();
  __resetCallLifecycleTelemetryForTesting();
  __setFaroForTesting(null);
});

describe("startDmCallAction", () => {
  test("reports a missing XMPP sender as one failed DM preflight", async () => {
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          events.push({ name, attributes });
        },
      },
    } as never);

    await startDmCallAction({
      peerBareJid: "bob@waddle.test",
      media: { audio: true, video: false },
      getSender: () => null,
    });

    expect($callState.get()).toEqual({ phase: "idle" });
    expect(events).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "failed",
        end_reason: "error",
        call_kind: "dm",
      }),
    }]);
  });

  test("sends a fresh XEP-0353 propose and marks the peer as ringing", async () => {
    const sender: CallWireSender = {
      send_call_propose: mock(async () => undefined),
    };

    await startDmCallAction({
      peerBareJid: "bob@waddle.test",
      media: { audio: true, video: true },
      getSender: () => sender,
      getInitiator: () => "alice@waddle.test/web",
    });

    expect(sender.send_call_propose).toHaveBeenCalledTimes(1);
    const [peerJid, sid, audio, video] = (sender.send_call_propose as {
      mock: { calls: unknown[][] };
    }).mock.calls[0] ?? [];
    expect(peerJid).toBe("bob@waddle.test");
    expect(typeof sid).toBe("string");
    expect(String(sid)).toMatch(/^c/);
    expect(audio).toBe(true);
    expect(video).toBe(true);
    expect($callState.get()).toMatchObject({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid,
      initiator: "alice@waddle.test/web",
    });
    expect(readDmCallActivity("bob@waddle.test")).toMatchObject({
      sid,
      state: "ringing",
      direction: "outgoing",
    });
  });

  test("rolls back optimistic state when the propose fails", async () => {
    const sender: CallWireSender = {
      send_call_propose: mock(async () => {
        throw new Error("wire down");
      }),
    };

    await startDmCallAction({
      peerBareJid: "bob@waddle.test",
      media: { audio: true, video: false },
      getSender: () => sender,
    });

    expect($callState.get()).toEqual({ phase: "idle" });
    expect(readDmCallActivity("bob@waddle.test")).toBeNull();
  });

  test("answers a hydrated incoming call with the stored proposer full JID", async () => {
    const now = new Date();
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: "bob@waddle.test/phone",
        sid: "hydrated-incoming",
        media: { audio: true, video: true },
      },
      selfBareJid: "alice@waddle.test",
      to: "alice@waddle.test/web",
      timestamp: now.toISOString(),
      now,
    });

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "bob@waddle.test/phone",
      sid: "hydrated-incoming",
      media: { audio: true, video: true },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "hydrated-incoming",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/phone",
      sid: "hydrated-incoming",
      media: { audio: true, video: true },
      accepting: true,
    });
  });

  test("does not answer a hydrated incoming call without a matching peer full JID", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "mallory@waddle.test/phone",
      sid: "forged",
      media: { audio: true, video: false },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("does not answer when the hydrated incoming activity was already cleared", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "propose",
        from: "bob@waddle.test/phone",
        sid: "stale-call",
        media: { audio: true, video: false },
      },
      selfBareJid: "alice@waddle.test",
      to: "alice@waddle.test/web",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });
    clearDmCallActivity("bob@waddle.test", "stale-call");

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "bob@waddle.test/phone",
      sid: "stale-call",
      media: { audio: true, video: false },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("answers an existing live incoming call through the current full JID", async () => {
    const sender: CallWireSender = {
      send_call_proceed: mock(async () => undefined),
    };
    $callState.set({
      phase: "incoming",
      from: "bob@waddle.test/laptop",
      sid: "same-call",
      media: { audio: true, video: false },
    });

    await answerIncomingDmCallActivity({
      peerBareJid: "bob@waddle.test",
      proposerFullJid: "bob@waddle.test/stale-phone",
      sid: "same-call",
      media: { audio: true, video: true },
      getSender: () => sender,
    });

    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/laptop",
      "same-call",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/laptop",
      sid: "same-call",
      media: { audio: true, video: false },
      accepting: true,
    });
  });

  test("resumes an accepted hydrated DM call when the LiveKit identity matches this resource", () => {
    const join = {
      url: "wss://livekit.waddle.test",
      room: "dm-call-restored",
      identity: "alice@waddle.test/web",
      token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "hydrated-live",
        media: { audio: true, video: true },
        join,
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    expect(resumeDmCallActivity({
      peerBareJid: "bob@waddle.test",
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).toBe(true);
    expect($callState.get()).toEqual({
      phase: "active",
      kind: "dm",
      peer: "bob@waddle.test/phone",
      sid: "hydrated-live",
      media: { audio: true, video: true },
      join,
      initiator: "alice@waddle.test/web",
    });
  });

  test("does not resume another resource's archived LiveKit identity", () => {
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/tablet",
        sid: "other-resource-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-other",
          identity: "alice@waddle.test/tablet",
          token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    expect(resumeDmCallActivity({
      peerBareJid: "bob@waddle.test",
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).toBe(false);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("does not resume archived LiveKit credentials before this resource is known", () => {
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "missing-local-resource",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-local",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    expect(resumeDmCallActivity({
      peerBareJid: "bob@waddle.test",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).toBe(false);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("does not resume archived LiveKit credentials after their token expires", () => {
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "expired-livekit-token",
        media: { audio: true, video: true },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-expired",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T11:59:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    expect(resumeDmCallActivity({
      peerBareJid: "bob@waddle.test",
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).toBe(false);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("does not resume archived LiveKit credentials without the peer full JID", () => {
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test",
        to: "alice@waddle.test/web",
        sid: "missing-peer-resource",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-no-peer-resource",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    expect(resumeDmCallActivity({
      peerBareJid: "bob@waddle.test",
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).toBe(false);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("ends a recovered same-resource DM call without rejoining media", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "recovered-live",
        media: { audio: true, video: true },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-recovered",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).resolves.toBe(true);

    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "recovered-live",
      "success",
    );
    expect(sender.send_call_finish).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "recovered-live",
    );
    expect(readDmCallActivity("bob@waddle.test")).toBeNull();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("ends a recovered same-resource DM call after its reconnect token expires", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "expired-recovered-live",
        media: { audio: true, video: true },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-expired",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T11:59:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).resolves.toBe(true);

    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "expired-recovered-live",
      "success",
    );
    expect(sender.send_call_finish).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "expired-recovered-live",
    );
    expect(readDmCallActivity("bob@waddle.test")).toBeNull();
  });

  test("ends a recovered DM call even while the local call slot is busy", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      send_call_finish: mock(async () => undefined),
    };
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: "room@muc.waddle.test",
      sid: "current-room",
      media: { audio: true, video: false },
      join: {
        url: "wss://livekit.waddle.test",
        room: "room@muc.waddle.test",
        identity: "alice@waddle.test/web",
        token: "opaque",
      },
    });
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "busy-recovered-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-busy",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T11:59:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    })).resolves.toBe(true);

    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "busy-recovered-live",
      "success",
    );
    expect($callState.get()).toMatchObject({
      phase: "active",
      kind: "muc",
      sid: "current-room",
    });
  });

  test("ends recovered DM activity when terminate is orphaned but finish sends", async () => {
    const now = new Date();
    const tokenExp = new Date(now.getTime() + 60 * 60 * 1000);
    const sender: CallWireSender = {
      send_call_session_terminate_with_outcome: mock(async () => ({ kind: "orphaned" })),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "finish-recovered-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-retry",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(tokenExp.getTime() / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: now.toISOString(),
      now,
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now,
    })).resolves.toBe(true);

    expect(sender.send_call_session_terminate_with_outcome).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "finish-recovered-live",
      "success",
    );
    expect(sender.send_call_finish).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "finish-recovered-live",
    );
    expect(readDmCallActivity("bob@waddle.test", now)).toBeNull();
  });

  test("keeps recovered DM activity retryable when no end marker sends", async () => {
    const now = new Date();
    const tokenExp = new Date(now.getTime() + 60 * 60 * 1000);
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => {
        throw new Error("terminate failed");
      }),
      send_call_finish: mock(async () => {
        throw new Error("finish failed");
      }),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "retry-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-retry",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(tokenExp.getTime() / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: now.toISOString(),
      now,
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now,
    })).resolves.toBe(false);

    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect(readDmCallActivity("bob@waddle.test", now)).toMatchObject({
      sid: "retry-live",
      state: "accepted",
    });
  });

  test("keeps recovered DM activity retryable when typed terminate outcome is error", async () => {
    const now = new Date();
    const tokenExp = new Date(now.getTime() + 60 * 60 * 1000);
    const sender: CallWireSender = {
      send_call_session_terminate_with_outcome: mock(async () => ({ kind: "error" })),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "typed-error-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-typed-error",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(tokenExp.getTime() / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: now.toISOString(),
      now,
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now,
    })).resolves.toBe(false);

    expect(sender.send_call_session_terminate_with_outcome).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "typed-error-live",
      "success",
    );
    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect(readDmCallActivity("bob@waddle.test", now)).toMatchObject({
      sid: "typed-error-live",
      state: "accepted",
    });
  });

  test("ends the requested same-resource sid without clearing another-resource activity", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "own-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-own",
          identity: "alice@waddle.test/web",
          token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:00:00.000Z",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/tablet",
        to: "alice@waddle.test/phone",
        sid: "other-resource-live",
        media: { audio: true, video: true },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-other",
          identity: "alice@waddle.test/phone",
          token: jwtWithExp(Date.parse("2026-05-26T13:00:00.000Z") / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: "2026-05-26T12:01:00.000Z",
      now: new Date("2026-05-26T12:01:00.000Z"),
    });

    expect(readDmCallActivity(
      "bob@waddle.test",
      new Date("2026-05-26T12:01:00.000Z"),
      "alice@waddle.test/web",
    )).toMatchObject({ sid: "own-live" });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      sid: "own-live",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now: new Date("2026-05-26T12:01:00.000Z"),
    })).resolves.toBe(true);

    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "own-live",
      "success",
    );
    expect(readDmCallActivity(
      "bob@waddle.test",
      new Date("2026-05-26T12:01:00.000Z"),
    )).toMatchObject({ sid: "other-resource-live" });
  });

  test("does not end another resource's recovered DM call", async () => {
    const now = new Date();
    const tokenExp = new Date(now.getTime() + 60 * 60 * 1000);
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/tablet",
        sid: "other-resource-live",
        media: { audio: true, video: false },
        join: {
          url: "wss://livekit.waddle.test",
          room: "dm-call-other-resource",
          identity: "alice@waddle.test/tablet",
          token: jwtWithExp(tokenExp.getTime() / 1000),
        },
      },
      selfBareJid: "alice@waddle.test",
      timestamp: now.toISOString(),
      now,
    });

    await expect(endRecoveredDmCallAction({
      peerBareJid: "bob@waddle.test",
      getSender: () => sender,
      getSelfFullJid: () => "alice@waddle.test/web",
      now,
    })).resolves.toBe(false);

    expect(sender.send_call_session_terminate).not.toHaveBeenCalled();
    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect(readDmCallActivity("bob@waddle.test", now)).toMatchObject({
      sid: "other-resource-live",
    });
  });
});
