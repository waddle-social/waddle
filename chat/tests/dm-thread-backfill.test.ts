// DM thread backfill: opening a DM thread (from the global Threads view, the
// DM sidebar thread list, or a deep link) must fetch the thread's replies
// from the personal archive via XEP-0313 MAM filtered by `with=peer` + the
// Waddle thread field — so threads whose replies fall outside the latest DM
// window still populate, with parity to channel thread backfill.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useDmMamPaging } from "../src/dms/mam-paging";
import type { BrowserXmppClient, LiveDmMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { TimelineMessage } from "../src/lib/chat-ui";

const session: WaddleSession = {
  username: "alice",
  jid: "alice@example.com/desktop",
  session_id: "tok",
  xmpp_websocket_url: "wss://example.com/ws",
};

const PEER = "bob@example.com";

function reply(id: string, threadId: string, body: string): LiveDmMessage {
  return {
    id,
    type: "message",
    peerJid: PEER,
    fromJid: "bob@example.com/phone",
    nick: "bob",
    body,
    timestamp: Date.now(),
    isSelf: false,
    threadId,
  } as unknown as LiveDmMessage;
}

function harness(xmppClient: BrowserXmppClient) {
  const messages = ref<TimelineMessage[]>([]);
  const actionError = ref("");
  const paging = useDmMamPaging({
    session: ref(session),
    xmppClient: ref(xmppClient),
    activePeerJid: ref(PEER),
    messages,
    firstUnseenId: ref<string | null>(null),
    loadErrorPeerJid: ref<string | null>(null),
    loadErrorMessage: ref(""),
    timelineEl: ref(null),
    scrollDirection: ref("bottom"),
    pinnedEdgeScroller: { disconnect: () => {} },
    actionError,
    clearActionError: () => {},
    pendingEchoClientIds: new Set<string>(),
    appendQueuedMessages: (timeline) => timeline,
    scrollToPinnedEdgeAndPin: async () => true,
    isFeedVisible: (m) => !m.threadId || m.id === m.threadId,
    persistLastSeen: () => {},
    dmLoadErrorMessage: () => "load error",
  });
  return { paging, messages, actionError };
}

describe("useDmMamPaging — DM thread backfill", () => {
  test("backfillThread fetches replies for a peer+thread and merges them", async () => {
    const queryPersonalMamThreadPage = mock(
      async (_peer: string, _thread: string, _max: number, _param: { type: string }) => ({
        messages: [reply("r1", "root-1", "first reply")],
        firstArchiveId: "arch-r1",
        complete: false,
      }),
    );
    const xmppClient = { queryPersonalMamThreadPage } as unknown as BrowserXmppClient;
    const { paging, messages } = harness(xmppClient);

    await paging.backfillThread("root-1");

    expect(queryPersonalMamThreadPage).toHaveBeenCalledTimes(1);
    expect(queryPersonalMamThreadPage.mock.calls[0]?.slice(0, 4)).toEqual([
      PEER,
      "root-1",
      100,
      { type: "latest" },
    ]);
    expect(messages.value.some((m) => m.id === "r1")).toBe(true);
    // Page was not complete and carried a first archive id → older replies.
    expect(paging.threadHasOlder.value["root-1"]).toBe(true);
  });

  test("backfillThread marks no-older when the latest page is complete", async () => {
    const xmppClient = {
      queryPersonalMamThreadPage: mock(async () => ({
        messages: [reply("r1", "root-1", "only reply")],
        firstArchiveId: "arch-r1",
        complete: true,
      })),
    } as unknown as BrowserXmppClient;
    const { paging } = harness(xmppClient);

    await paging.backfillThread("root-1");

    expect(paging.threadHasOlder.value["root-1"]).toBe(false);
  });

  test("loadOlderThreadMessages pages older replies with a before-cursor", async () => {
    const queryPersonalMamThreadPage = mock(
      async (_peer: string, _thread: string, _max: number, param: { type: string; before?: string }) => {
        if (param.type === "before") {
          expect(param.before).toBe("arch-r1");
          return { messages: [reply("r0", "root-1", "older reply")], firstArchiveId: "arch-r0", complete: true };
        }
        return { messages: [reply("r1", "root-1", "first reply")], firstArchiveId: "arch-r1", complete: false };
      },
    );
    const xmppClient = { queryPersonalMamThreadPage } as unknown as BrowserXmppClient;
    const { paging, messages } = harness(xmppClient);

    await paging.backfillThread("root-1");
    await paging.loadOlderThreadMessages("root-1");

    expect(messages.value.some((m) => m.id === "r0")).toBe(true);
    expect(messages.value.some((m) => m.id === "r1")).toBe(true);
    expect(paging.threadHasOlder.value["root-1"]).toBe(false);
    expect(paging.loadingOlderThreadIds.value.has("root-1")).toBe(false);
  });

  test("backfillThread no-ops when the client lacks the thread query", async () => {
    const xmppClient = {} as unknown as BrowserXmppClient;
    const { paging, messages } = harness(xmppClient);
    await paging.backfillThread("root-1");
    expect(messages.value).toHaveLength(0);
    expect(paging.threadHasOlder.value["root-1"]).toBeUndefined();
  });
});
