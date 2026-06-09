import { afterEach, describe, expect, test } from "bun:test";
import { $callState } from "../src/lib/calls/call-store";
import { $callUiMode } from "../src/lib/calls/ui-mode";
import { $mucCallParticipants } from "../src/lib/calls/muc-call-presence";
import { connectionStore } from "../src/lib/connection-store";
import { resolveActiveCallThreadId, resolveActiveMucCallThreadId } from "../src/lib/calls/call-chat-composer";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { renderVueComponent } from "./helpers/render-vue-sfc";

const ROOM = "lobby@muc.waddle.test";

describe("call-chat composer", () => {
  afterEach(() => {
    $callState.set({ phase: "idle" });
    $callUiMode.set("split");
    $mucCallParticipants.set({});
    connectionStore.session = null;
  });

  test("resolves the active MUC call thread from the current conversation timeline", () => {
    const messages: TimelineMessage[] = [
      message({ id: "ordinary", body: "hello" }),
      message({
        id: "call-anchor",
        threadId: "call-thread-123",
        callThread: {
          kind: "muc",
          sid: "sid-active",
          media: ["audio"],
          initiator: "alice@waddle.test/web",
          started: "2026-06-09T12:00:00Z",
        },
      }),
      message({
        id: "other-call-anchor",
        threadId: "call-thread-older",
        callThread: {
          kind: "muc",
          sid: "sid-old",
          media: ["audio"],
          initiator: "alice@waddle.test/web",
          started: "2026-06-09T11:00:00Z",
        },
      }),
    ];

    expect(resolveActiveMucCallThreadId(messages, `${ROOM}/alice`, "sid-active")).toBe("call-thread-123");
  });

  test("does not resolve when the active room or sid is unusable", () => {
    const messages = [
      message({
        id: "call-anchor",
        threadId: "call-thread-123",
        callThread: { kind: "muc", sid: "sid-active", media: ["audio"] },
      }),
    ];

    expect(resolveActiveMucCallThreadId(messages, "", "sid-active")).toBeNull();
    expect(resolveActiveMucCallThreadId(messages, ROOM, " ")).toBeNull();
  });

  test("resolves the active DM call thread from the current conversation timeline", () => {
    const messages: TimelineMessage[] = [
      message({ id: "ordinary", body: "hello" }),
      message({
        id: "dm-call-anchor",
        threadId: "dm-call-thread-123",
        callThread: {
          kind: "dm",
          sid: "dm-active",
          media: ["audio"],
          initiator: "alice@waddle.test",
          started: "2026-06-09T12:00:00Z",
        },
      }),
    ];

    expect(resolveActiveCallThreadId(messages, {
      kind: "dm",
      peerJid: "bob@waddle.test",
      sid: "dm-active",
    })).toBe("dm-call-thread-123");
    expect(resolveActiveCallThreadId(messages, {
      kind: "dm",
      sid: "dm-active",
    })).toBe("dm-call-thread-123");
  });

  test("expanded channel calls render a labelled call-chat composer when a call thread is available", async () => {
    seedExpandedMucCall();

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
      },
      import.meta.url,
    );

    expect(html).toContain("Call chat");
    expect(html).toContain('aria-label="Call chat composer"');
    expect(html).not.toContain('aria-label="Extensions"');
  });

  test("expanded channel calls hide the call-chat composer without a usable thread id", async () => {
    seedExpandedMucCall();

    const html = await renderVueComponent(
      "../src/components/calls/CallExpandedSurface.vue",
      {
        roomJid: ROOM,
        callThreadId: "   ",
        uploadProgress: { uploading: false, progress: 0, filename: "" },
      },
      import.meta.url,
    );

    expect(html).not.toContain("Call chat composer");
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
