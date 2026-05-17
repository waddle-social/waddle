// Tests for the click-thru behaviour of the global Threads view.
//
// `ThreadsView.vue` is a thin wrapper: it fetches threads (delegated
// to `ThreadsListPanel`) and, on click, hands the entry off to the
// controller's `onSelectThread(channelId, threadId)`. The controller
// then selects the channel and opens the ThreadPanel reader, and the
// existing URL watchers push `/r/<channel>?thread=<rootId>`.
//
// The component itself is too small to be worth mounting under
// `@vue/test-utils`; we exercise the two pieces of behaviour that
// matter: the JID→channel-id resolver and the controller-call shape.

import { describe, expect, mock, test } from "bun:test";
import type { WasmThreadEntry } from "../src/lib/xmpp/wasm-types";
import { resolveChannelIdForRoomJid } from "../src/lib/threads-channel-resolve";

function entry(overrides: Partial<WasmThreadEntry> = {}): WasmThreadEntry {
  return {
    channel: "general@muc.waddle.example",
    thread_id: "root-1",
    last_stanza_id: "S",
    last_activity: new Date().toISOString(),
    unread: 0,
    reply_count: 0,
    has_unread: false,
    ...overrides,
  };
}

/**
 * Mirrors the click handler inside `ThreadsView.vue`: resolve the
 * entry's room JID to a channel-id slug, then call `onSelectThread`.
 * If the JID is unusable, the handler is a no-op.
 */
async function handleOpenThread(
  e: WasmThreadEntry,
  channels: { id: string; jid?: string }[],
  onSelectThread: (channelId: string, threadId: string) => void | Promise<void>,
): Promise<void> {
  const channelId = resolveChannelIdForRoomJid(e.channel, channels);
  if (!channelId) return;
  await onSelectThread(channelId, e.thread_id);
}

describe("resolveChannelIdForRoomJid", () => {
  test("matches a channel by exact bare JID", () => {
    const channels = [
      { id: "general", jid: "general@muc.waddle.example" },
      { id: "random", jid: "random@muc.waddle.example" },
    ];
    expect(
      resolveChannelIdForRoomJid("general@muc.waddle.example", channels),
    ).toBe("general");
  });

  test("ignores resource on the room JID when matching", () => {
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    expect(
      resolveChannelIdForRoomJid(
        "general@muc.waddle.example/some-nick",
        channels,
      ),
    ).toBe("general");
  });

  test("falls back to the JID local-part when no channel matches", () => {
    expect(
      resolveChannelIdForRoomJid("orphan@muc.waddle.example", []),
    ).toBe("orphan");
  });

  test("returns null for an empty / malformed JID", () => {
    expect(resolveChannelIdForRoomJid("", [])).toBeNull();
    expect(resolveChannelIdForRoomJid("@muc.waddle.example", [])).toBeNull();
  });

  test("prefers an explicit channel match over the local-part fallback", () => {
    // Two channels share the same node — the explicit `jid` match wins,
    // so we route to `pretty-name` rather than the bare slug `general`.
    const channels = [
      { id: "pretty-name", jid: "general@muc.waddle.example" },
      { id: "general", jid: "general@other-host.example" },
    ];
    expect(
      resolveChannelIdForRoomJid("general@muc.waddle.example", channels),
    ).toBe("pretty-name");
  });
});

describe("ThreadsView click handler", () => {
  test("calls onSelectThread with the resolved channel-id and the thread id", async () => {
    const onSelectThread = mock(async (_channelId: string, _threadId: string) => {});
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    await handleOpenThread(
      entry({ channel: "general@muc.waddle.example", thread_id: "root-abc" }),
      channels,
      onSelectThread,
    );
    expect(onSelectThread).toHaveBeenCalledTimes(1);
    expect(onSelectThread.mock.calls[0]).toEqual(["general", "root-abc"]);
  });

  test("falls back to the JID local-part when the channel is unknown", async () => {
    // Threads view can outlive the structure load: a thread can be
    // listed before its channel summary has been wired into
    // `waddles.channels`. The handler must still route correctly.
    const onSelectThread = mock(async (_channelId: string, _threadId: string) => {});
    await handleOpenThread(
      entry({ channel: "history@muc.waddle.example", thread_id: "t-9" }),
      [],
      onSelectThread,
    );
    expect(onSelectThread).toHaveBeenCalledTimes(1);
    expect(onSelectThread.mock.calls[0]).toEqual(["history", "t-9"]);
  });

  test("is a no-op for entries with an unusable channel JID", async () => {
    const onSelectThread = mock(async (_channelId: string, _threadId: string) => {});
    await handleOpenThread(entry({ channel: "" }), [], onSelectThread);
    expect(onSelectThread).not.toHaveBeenCalled();
  });

  test("awaits onSelectThread so async controller work completes before returning", async () => {
    let resolved = false;
    const onSelectThread = mock(async () => {
      await new Promise<void>((r) => setTimeout(r, 1));
      resolved = true;
    });
    await handleOpenThread(entry(), [], onSelectThread);
    expect(resolved).toBe(true);
  });
});
