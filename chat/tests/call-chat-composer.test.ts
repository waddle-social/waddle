import { afterEach, describe, expect, test } from "bun:test";
import { $callState } from "../src/lib/calls/call-store";
import { $callUiMode } from "../src/lib/calls/ui-mode";
import {
  closeCallDock,
  openCallDock,
  setCallDockTab,
} from "../src/lib/calls/call-dock-state";
import { $mucCallParticipants } from "../src/lib/calls/muc-call-presence";
import { connectionStore } from "../src/lib/connection-store";
import { activeCallChatThreadId, resolveActiveMucCallThreadId } from "../src/lib/calls/call-chat-composer";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { renderVueComponent } from "./helpers/render-vue-sfc";

const ROOM = "lobby@muc.waddle.test";

describe("call-chat composer", () => {
  afterEach(() => {
    $callState.set({ phase: "idle" });
    $callUiMode.set("split");
    closeCallDock();
    setCallDockTab("participants");
    $mucCallParticipants.set({});
    connectionStore.session = null;
  });

  test("resolves the active MUC call thread by room, skipping ended calls", () => {
    // The MUC anchor's marker sid is a throwaway server-side uuid that never
    // matches the client's per-attempt call sid, so resolution is by room:
    // the most recent non-ended `muc` anchor in the room's timeline.
    const messages: TimelineMessage[] = [
      message({ id: "ordinary", body: "hello" }),
      message({
        id: "ended-call-anchor",
        threadId: "call-thread-old",
        callThread: {
          kind: "muc",
          sid: "sid-old",
          media: ["audio"],
          initiator: "alice@waddle.test/web",
          started: "2026-06-09T11:00:00Z",
          ended: "2026-06-09T11:30:00Z",
        },
      }),
      message({
        id: "active-call-anchor",
        threadId: "call-thread-active",
        callThread: {
          kind: "muc",
          sid: "sid-fresh",
          media: ["audio"],
          initiator: "alice@waddle.test/web",
          started: "2026-06-09T12:00:00Z",
        },
      }),
    ];

    expect(resolveActiveMucCallThreadId(messages, `${ROOM}/alice`)).toBe("call-thread-active");
  });

  test("skips a more recent ended muc anchor to find the still-active one", () => {
    // The ended anchor is the most recent row, so the reverse scan must skip
    // it (not just stop at the newest) to return the still-active call.
    const messages: TimelineMessage[] = [
      message({
        id: "active-call-anchor",
        threadId: "call-thread-active",
        callThread: { kind: "muc", sid: "sid-fresh", media: ["audio"] },
      }),
      message({
        id: "ended-call-anchor",
        threadId: "call-thread-old",
        callThread: { kind: "muc", sid: "sid-old", media: ["audio"], ended: "2026-06-09T13:00:00Z" },
      }),
    ];

    expect(resolveActiveMucCallThreadId(messages, `${ROOM}/alice`)).toBe("call-thread-active");
  });

  test("does not resolve a MUC call thread without a room or an active anchor", () => {
    const active: TimelineMessage[] = [
      message({
        id: "active-call-anchor",
        threadId: "call-thread-active",
        callThread: { kind: "muc", sid: "sid-fresh", media: ["audio"] },
      }),
    ];
    const endedOnly: TimelineMessage[] = [
      message({
        id: "ended-call-anchor",
        threadId: "call-thread-old",
        callThread: { kind: "muc", sid: "sid-old", media: ["audio"], ended: "2026-06-09T11:30:00Z" },
      }),
    ];

    expect(resolveActiveMucCallThreadId(active, "")).toBeNull();
    expect(resolveActiveMucCallThreadId(endedOnly, ROOM)).toBeNull();
  });

  test("derives the active DM call-chat thread id from the call sid", () => {
    // Server invariant: a DM call-thread id equals the JMI session id, which
    // is exactly the client's active call sid — so the in-call composer can
    // resolve the thread live without waiting for a timeline anchor.
    expect(
      activeCallChatThreadId({ phase: "active", kind: "dm", sid: "dm-sid-1" }, [], null),
    ).toBe("dm-sid-1");
    expect(
      activeCallChatThreadId({ phase: "active", kind: "dm", sid: "  " }, [], null),
    ).toBeNull();
  });

  test("derives the active MUC call-chat thread id from the room timeline", () => {
    const messages: TimelineMessage[] = [
      message({
        id: "active-call-anchor",
        threadId: "call-thread-active",
        callThread: { kind: "muc", sid: "sid-fresh", media: ["audio"] },
      }),
    ];

    expect(
      activeCallChatThreadId({ phase: "active", kind: "muc", sid: "sid-fresh" }, messages, ROOM),
    ).toBe("call-thread-active");
    expect(
      activeCallChatThreadId({ phase: "idle" }, messages, ROOM),
    ).toBeNull();
  });

  test("expanded channel calls render a labelled call-chat composer in the dock Chat tab", async () => {
    seedExpandedMucCall();
    // Chat now lives in the dock's Chat tab, so open the dock there to render it.
    setCallDockTab("chat");
    openCallDock();

    const html = await renderVueComponent(
      "../src/components/calls/CallExpandedSurface.vue",
      {
        roomJid: ROOM,
        callThreadId: "call-thread-123",
        isSending: false,
        disabled: false,
        giphyApiKey: "",
        mentionCandidates: [],
        slowModeCooldown: 0,
        uploadProgress: { uploading: false, progress: 0, filename: "" },
        callChatMessages: [],
        avatarUrlByAuthor: {},
      },
      import.meta.url,
    );

    expect(html).toContain('aria-label="Call chat composer"');
    expect(html).not.toContain('aria-label="Extensions"');
    // With a usable thread the composer is enabled (not greyed out).
    expect(html).not.toContain("pointer-events-none");
  });

  test("expanded DM calls render the call-chat composer from the call sid", async () => {
    seedExpandedDmCall();
    setCallDockTab("chat");
    openCallDock();

    const html = await renderVueComponent(
      "../src/components/calls/CallExpandedSurface.vue",
      {
        dmPeerJid: "bob@waddle.test",
        dmPeerName: "Bob",
        callThreadId: "dm-sid-1",
        uploadProgress: { uploading: false, progress: 0, filename: "" },
        callChatMessages: [],
        avatarUrlByAuthor: {},
      },
      import.meta.url,
    );

    expect(html).toContain('aria-label="Call chat composer"');
  });

  test("expanded channel calls disable the call-chat composer without a usable thread id", async () => {
    seedExpandedMucCall();
    setCallDockTab("chat");
    openCallDock();

    const html = await renderVueComponent(
      "../src/components/calls/CallExpandedSurface.vue",
      {
        roomJid: ROOM,
        callThreadId: "   ",
        uploadProgress: { uploading: false, progress: 0, filename: "" },
        callChatMessages: [],
        avatarUrlByAuthor: {},
      },
      import.meta.url,
    );

    // The composer stays in the tab but is disabled until a thread resolves.
    expect(html).toContain('aria-label="Call chat composer"');
    expect(html).toContain("pointer-events-none");
  });
});

function seedExpandedMucCall(): void {
  connectionStore.session = {
    username: "alice",
    jid: "alice@waddle.test/browser",
  } as typeof connectionStore.session;
  $mucCallParticipants.set({ [ROOM]: ["alice"] });
  $callUiMode.set("expanded");
  $callState.set({
    phase: "active",
    kind: "muc",
    peer: ROOM,
    sid: "sid-active",
    media: { audio: true, video: false },
    join: {
      url: "wss://livekit.waddle.test",
      room: ROOM,
      identity: "alice",
      token: "token",
    },
    selfNick: "alice",
    selfFullJid: "alice@waddle.test/browser",
  });
}

function seedExpandedDmCall(): void {
  connectionStore.session = {
    username: "alice",
    jid: "alice@waddle.test/browser",
  } as typeof connectionStore.session;
  $callUiMode.set("expanded");
  $callState.set({
    phase: "active",
    kind: "dm",
    peer: "bob@waddle.test/desktop",
    sid: "dm-sid-1",
    media: { audio: true, video: false },
    join: {
      url: "wss://livekit.waddle.test",
      room: "dm-sid-1",
      identity: "alice",
      token: "token",
    },
    initiator: "alice@waddle.test/browser",
  });
}

function message(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "message",
    body: "",
    author: "alice",
    createdAt: "2026-06-09T12:00:00Z",
    createdAtSource: "archive",
    isSelf: false,
    ...overrides,
  };
}
