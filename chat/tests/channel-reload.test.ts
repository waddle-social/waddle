import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useWaddleDirectory } from "../src/waddles/directory";
import type { MemberSummary } from "../src/lib/chat-types";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";

const ALICE: MemberSummary = { jid: "alice@example.com", username: "alice", avatar_url: null, affiliation: "member", joined_at: "" };
const BOB: MemberSummary = { jid: "bob@example.com", username: "bob", avatar_url: null, affiliation: "member", joined_at: "" };

const BASE_TOPOLOGY = {
  spaces: [],
  rooms: [
    { id: "general", name: "General", jid: "general@conference.example.com", channelType: "text" as const, position: 0 },
    { id: "random", name: "Random", jid: "random@conference.example.com", channelType: "text" as const, position: 1 },
  ],
};

function makeClient(overrides: {
  listRoomMembers?: (id: string, opts?: { roomJid?: string }) => Promise<MemberSummary[]>;
  discoverTopology?: () => Promise<unknown>;
} = {}): BrowserXmppClient {
  return {
    listRoomMembers: overrides.listRoomMembers ?? mock(async () => []),
    discoverTopology: overrides.discoverTopology ?? mock(async () => BASE_TOPOLOGY),
    communityProvisioning: {},
  } as unknown as BrowserXmppClient;
}

function makeWaddles(client: BrowserXmppClient | null = null) {
  const xmppClient = ref<BrowserXmppClient | null>(client);
  const actionError = ref("");
  const clearActionError = mock(() => { actionError.value = ""; });
  const normalizeError = (e: unknown) => (e instanceof Error ? e.message : String(e));

  const w = useWaddleDirectory(
    xmppClient,
    ref(null),
    normalizeError,
    actionError,
    clearActionError,
  );

  return { w, xmppClient, actionError };
}

describe("useWaddleDirectory.loadStructure", () => {
  test("loads members for the first channel when no preferred channel is supplied", async () => {
    const listRoomMembers = mock(async (_id: string, _opts?: { roomJid?: string }) => [ALICE]);
    const client = makeClient({ listRoomMembers });
    const { w } = makeWaddles(client);

    await w.loadStructure();

    expect(listRoomMembers).toHaveBeenCalledTimes(1);
    expect(listRoomMembers.mock.calls[0]![0]).toBe("general");
    expect(listRoomMembers.mock.calls[0]![1]).toEqual({ roomJid: "general@conference.example.com" });
    expect(w.members.value).toEqual([ALICE]);
    expect(w.memberLoadState.value).toBe("ready");
  });

  test("loads members for the preferred channel when a channelId is supplied", async () => {
    const listRoomMembers = mock(async (_id: string, _opts?: { roomJid?: string }) => [BOB]);
    const client = makeClient({ listRoomMembers });
    const { w } = makeWaddles(client);

    const returned = await w.loadStructure("random");

    expect(returned).toBe("random");
    expect(listRoomMembers).toHaveBeenCalledTimes(1);
    expect(listRoomMembers.mock.calls[0]![0]).toBe("random");
    expect(listRoomMembers.mock.calls[0]![1]).toEqual({ roomJid: "random@conference.example.com" });
    expect(w.members.value).toEqual([BOB]);
    expect(w.activeChannelId.value).toBe("random");
    expect(w.memberLoadState.value).toBe("ready");
  });

  test("routes to the first channel when preferred channel is not in the topology", async () => {
    const listRoomMembers = mock(async (_id: string, _opts?: { roomJid?: string }) => [ALICE]);
    const client = makeClient({ listRoomMembers });
    const { w } = makeWaddles(client);

    const returned = await w.loadStructure("does-not-exist");

    expect(returned).toBe("general");
    expect(listRoomMembers.mock.calls[0]![0]).toBe("general");
    expect(w.activeChannelId.value).toBe("general");
  });

  test("keeps all channels visible and derives the active space from the selected channel", async () => {
    const client = makeClient({
      discoverTopology: mock(async () => ({
        spaces: [
          { id: "alpha", name: "Alpha" },
          { id: "beta", name: "Beta" },
        ],
        rooms: [
          { id: "general", name: "General", jid: "general@conference.example.com", channelType: "text" as const, position: 0, spaceId: "alpha" },
          { id: "random", name: "Random", jid: "random@conference.example.com", channelType: "text" as const, position: 1, spaceId: "beta" },
        ],
      })),
    });
    const { w } = makeWaddles(client);

    await w.loadStructure("random");

    expect(w.sortedChannels.value.map((channel) => channel.id)).toEqual(["general", "random"]);
    expect(w.currentSpace.value?.id).toBe("beta");
  });
});

describe("useWaddleDirectory.reloadChannelMembers", () => {
  test("loads members for the active channel using its discovered JID", async () => {
    const listRoomMembers = mock(async (_id: string, _opts?: { roomJid?: string }) => [ALICE]);
    const client = makeClient({ listRoomMembers });
    const { w } = makeWaddles(client);

    // Seed channels so the JID can be resolved
    await w.loadStructure();

    listRoomMembers.mockClear();
    await w.reloadChannelMembers("random");

    expect(listRoomMembers).toHaveBeenCalledTimes(1);
    expect(listRoomMembers.mock.calls[0]![0]).toBe("random");
    expect(listRoomMembers.mock.calls[0]![1]).toEqual({ roomJid: "random@conference.example.com" });
    expect(w.members.value).toEqual([ALICE]);
  });

  test("preserves previous members on failure and marks members unavailable", async () => {
    const warn = mock(() => {});
    const previousWarn = console.warn;
    console.warn = warn;
    const listRoomMembers = mock(async (_id: string) => [BOB]);
    const client = makeClient({ listRoomMembers });
    const { w, actionError } = makeWaddles(client);

    try {
      await w.loadStructure();
      // Seed members for the active channel
      await w.reloadChannelMembers("general");
      expect(w.members.value).toEqual([BOB]);

      // Now make it fail
      listRoomMembers.mockImplementation(async () => { throw new Error("forbidden"); });

      await w.reloadChannelMembers("general");

      // Members preserved, failure represented in member-specific state.
      expect(w.members.value).toEqual([BOB]);
      expect(w.memberLoadState.value).toBe("unavailable");
      expect(actionError.value).toBe("");
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      console.warn = previousWarn;
    }
  });

  test("discards stale result when channel switches rapidly", async () => {
    let resolveGeneral!: (v: MemberSummary[]) => void;
    let resolveRandom!: (v: MemberSummary[]) => void;

    const generalPromise = new Promise<MemberSummary[]>((res) => { resolveGeneral = res; });
    const randomPromise = new Promise<MemberSummary[]>((res) => { resolveRandom = res; });

    const listRoomMembers = mock(async (id: string) => {
      if (id === "general") return generalPromise;
      return randomPromise;
    });

    const { w } = makeWaddles(makeClient({ listRoomMembers }));

    // Seed channels directly — avoids loadStructure calling listRoomMembers and blocking
    w.channels.value = [
      { id: "general", name: "General", jid: "general@conference.example.com", channel_type: "text" },
      { id: "random", name: "Random", jid: "random@conference.example.com", channel_type: "text" },
    ];

    // Fire general reload, then immediately fire random (newer request wins)
    const generalReload = w.reloadChannelMembers("general");
    const randomReload = w.reloadChannelMembers("random");

    // Resolve random first — it is the newest request and should win
    resolveRandom([BOB]);
    await randomReload;
    expect(w.members.value).toEqual([BOB]);

    // Now resolve the older general request — stale, must be discarded
    resolveGeneral([ALICE]);
    await generalReload;
    expect(w.members.value).toEqual([BOB]);
  });

  test("does nothing when xmppClient is not connected", async () => {
    const { w, actionError } = makeWaddles(null);

    await w.reloadChannelMembers("general");

    expect(w.members.value).toEqual([]);
    expect(w.memberLoadState.value).toBe("idle");
    expect(actionError.value).toBe("");
  });
});
