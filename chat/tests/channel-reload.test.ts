import { afterEach, describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { withFakeDomParser } from "./helpers/disco-xml";
import { useWaddleDirectory } from "../src/waddles/directory";
import type { MemberSummary, RoomBotSummary } from "../src/lib/chat-types";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";

const ALICE: MemberSummary = { jid: "alice@example.com", username: "alice", affiliation: "member", joined_at: "" };
const BOB: MemberSummary = { jid: "bob@example.com", username: "bob", affiliation: "member", joined_at: "" };

const BASE_TOPOLOGY = {
  spaces: [],
  rooms: [
    { id: "general", name: "General", jid: "general@conference.example.com", channelType: "text" as const, position: 0 },
    { id: "random", name: "Random", jid: "random@conference.example.com", channelType: "text" as const, position: 1 },
  ],
};

function makeClient(overrides: {
  listRoomMembers?: (id: string, opts?: { roomJid?: string }) => Promise<MemberSummary[]>;
  listRoomBots?: (id: string, opts?: { roomJid?: string }) => Promise<RoomBotSummary[]>;
  discoverTopology?: () => Promise<unknown>;
} = {}): BrowserXmppClient {
  return {
    listRoomMembers: overrides.listRoomMembers ?? mock(async () => []),
    listRoomBots: overrides.listRoomBots ?? mock(async () => []),
    discoverTopology: overrides.discoverTopology ?? mock(async () => BASE_TOPOLOGY),
    agent: null,
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
  test.each([null, "random"])("keeps a newer selection (%s) when discovery completes", async (selection) => {
    let resolveDiscovery!: (value: typeof BASE_TOPOLOGY) => void;
    const discovery = new Promise<typeof BASE_TOPOLOGY>((resolve) => { resolveDiscovery = resolve; });
    const listRoomMembers = mock(async () => [ALICE]);
    const { w } = makeWaddles(makeClient({ discoverTopology: () => discovery, listRoomMembers }));
    w.activeChannelId.value = "general";

    const reload = w.loadStructure("general");
    w.activeChannelId.value = selection;
    resolveDiscovery(BASE_TOPOLOGY);
    await reload;

    expect(w.activeChannelId.value).toBe(selection);
    expect(w.channels.value.map((channel) => channel.id)).toEqual(["general", "random"]);
    expect(w.hasLoadedStructure.value).toBe(true);
    expect(w.isLoadingStructure.value).toBe(false);
    expect(listRoomMembers).not.toHaveBeenCalled();
  });

  test("does not clear a channel selected after a no-selection refresh starts, even after returning to it", async () => {
    let resolveDiscovery!: (value: typeof BASE_TOPOLOGY) => void;
    const discovery = new Promise<typeof BASE_TOPOLOGY>((resolve) => { resolveDiscovery = resolve; });
    const { w } = makeWaddles(makeClient({ discoverTopology: () => discovery }));
    w.activeChannelId.value = "general";

    const reload = w.loadStructure(null, { noChannelSelect: true });
    w.activeChannelId.value = null;
    w.activeChannelId.value = "general";
    resolveDiscovery(BASE_TOPOLOGY);
    await reload;

    expect(w.activeChannelId.value).toBe("general");
  });

  test("preserves a confirmed group DM during a background refresh", async () => {
    const group = { id: "crew", name: "Crew", jid: "crew@conference.example.com", channelType: "text" as const, isGroupDm: true };
    const listRoomMembers = mock(async () => [BOB]);
    const { w } = makeWaddles(makeClient({
      discoverTopology: async () => ({ ...BASE_TOPOLOGY, rooms: [...BASE_TOPOLOGY.rooms, group] }),
      listRoomMembers,
    }));
    w.activeChannelId.value = group.id;

    await w.loadStructure(group.id);

    expect(w.activeChannelId.value).toBe(group.id);
    expect(w.currentChannel.value?.isGroupDm).toBe(true);
    expect(w.sortedChannels.value.map((channel) => channel.id)).toEqual(["general", "random"]);
    expect(listRoomMembers).toHaveBeenCalledWith(group.id, { roomJid: group.jid });
  });

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
  test("ignores members from before leaving and returning to the same channel", async () => {
    let finishLoad!: (members: MemberSummary[]) => void;
    const load = new Promise<MemberSummary[]>((resolve) => { finishLoad = resolve; });
    const { w } = makeWaddles(makeClient({ listRoomMembers: () => load }));
    w.activeChannelId.value = "general";
    const pendingMembers = w.reloadChannelMembers("general");

    w.activeChannelId.value = null;
    w.activeChannelId.value = "general";
    finishLoad([ALICE]);
    await pendingMembers;

    expect(w.members.value).toEqual([]);
  });

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

describe("useWaddleDirectory room bots", () => {
  afterEach(() => occupantJidDirectory.clear());
  const HELPER: RoomBotSummary = { jid: "helper@extensions.example.com", name: "Helper" };
  const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  test("loads the focused room's declared bots with its members and tells the occupant directory", async () => {
    const listRoomBots = mock(async (_id: string, _opts?: { roomJid?: string }) => [HELPER]);
    const { w } = makeWaddles(makeClient({ listRoomBots }));

    await w.loadStructure("random");
    await flush();

    expect(listRoomBots).toHaveBeenCalledWith("random", { roomJid: "random@conference.example.com" });
    expect(w.roomBots.value).toEqual([HELPER]);
    expect(occupantJidDirectory.isBot(HELPER.jid)).toBe(true);

    // Bots are per room: another room has none until it is loaded.
    w.activeChannelId.value = "general";
    expect(w.roomBots.value).toEqual([]);
  });

  test("a failed bot load keeps the last list and never fails the member load", async () => {
    const warn = mock(() => {});
    const previousWarn = console.warn;
    console.warn = warn;
    const listRoomBots = mock(async (_id: string): Promise<RoomBotSummary[]> => [HELPER]);
    const { w } = makeWaddles(makeClient({ listRoomBots, listRoomMembers: async () => [ALICE] }));

    try {
      await w.loadStructure();
      await flush();
      listRoomBots.mockImplementation(async () => { throw new Error("service-unavailable"); });

      await w.reloadChannelMembers("general");
      await flush();

      expect(w.roomBots.value).toEqual([HELPER]);
      expect(w.members.value).toEqual([ALICE]);
      expect(w.memberLoadState.value).toBe("ready");
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      console.warn = previousWarn;
    }
  });

  test("a bot-hatted occupant missing from the focused room's list asks the server again; a listed or foreign one does not", async () => {
    const listRoomBots = mock(async (_id: string, _opts?: { roomJid?: string }) => [HELPER]);
    const { w } = makeWaddles(makeClient({ listRoomBots }));
    await w.loadStructure();
    await flush();
    listRoomBots.mockClear();

    w.reloadRoomBotsIfUnlisted("general@conference.example.com", HELPER.jid.toUpperCase());
    w.reloadRoomBotsIfUnlisted("random@conference.example.com", "new@extensions.example.com");
    await flush();
    expect(listRoomBots).not.toHaveBeenCalled();

    w.reloadRoomBotsIfUnlisted("general@conference.example.com", "new@extensions.example.com");
    await flush();
    expect(listRoomBots).toHaveBeenCalledTimes(1);
    expect(listRoomBots).toHaveBeenCalledWith("general", { roomJid: "general@conference.example.com" });
  });

  test("a reconnect reloads the focused room's bots", async () => {
    const listRoomBots = mock(async (_id: string, _opts?: { roomJid?: string }) => [HELPER]);
    const { w } = makeWaddles(makeClient({ listRoomBots }));
    await w.loadStructure();
    await flush();
    listRoomBots.mockClear();

    w.reloadFocusedRoomBots();
    await flush();

    expect(listRoomBots).toHaveBeenCalledWith("general", { roomJid: "general@conference.example.com" });
  });
});

describe("directory client lifetime", () => {
  const owner: MemberSummary = { ...ALICE, affiliation: "owner" };
  const bot: RoomBotSummary = { jid: "bot@example.com", name: "Old bot" };
  const topology = (service: string) => ({
    ...BASE_TOPOLOGY,
    serverRole: service === "a.test" ? "owner" : null,
    services: { muc: `muc.${service}`, spaces: `spaces.${service}` },
    spaces: [{ id: "space", name: service, role: null }],
    rooms: [{ id: "general", name: service, jid: `general@muc.${service}`, spaceId: "space" }],
  });
  function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: Error) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
  }
  const settle = async () => { for (let n = 0; n < 12; n++) await Promise.resolve(); };

  test.each(["success", "failure"])("replacement clears cached owner and bots during delayed %s", async (outcome) => {
    const oldMembers = deferred<MemberSummary[]>();
    const oldBots = deferred<RoomBotSummary[]>();
    let delayed = false;
    const old = makeClient({
      discoverTopology: async () => topology("a.test"),
      listRoomMembers: () => delayed ? oldMembers.promise : Promise.resolve([owner]),
      listRoomBots: () => delayed ? oldBots.promise : Promise.resolve([bot]),
    });
    const xmppClient = ref<BrowserXmppClient | null>(old);
    const actionError = ref("");
    const w = useWaddleDirectory(xmppClient, ref({ jid: ALICE.jid } as import("../src/lib/server-auth").WaddleSession), String, actionError, () => {});
    await w.loadStructure();
    expect(w.canManageMembers.value).toBe(true);
    expect(w.roomBots.value).toEqual([bot]);
    // Same-client reload deliberately retains its last known roster.
    delayed = true;
    const stale = w.reloadChannelMembers("general");
    expect(w.members.value).toEqual([owner]);
    w.editChannelForm.value.name = "Old edit";
    w.createChannelForm.value.name = "Old creation";
    const members = deferred<MemberSummary[]>();
    const bots = deferred<RoomBotSummary[]>();
    xmppClient.value = makeClient({
      discoverTopology: async () => topology("b.test"),
      listRoomMembers: () => members.promise,
      listRoomBots: () => bots.promise,
    });
    expect(w.members.value).toEqual([]);
    expect(w.roomBots.value).toEqual([]);
    expect(w.channels.value).toEqual([]);
    expect(w.waddles.value).toEqual([]);
    expect(w.groupDms.value).toEqual([]);
    expect(w.activeChannelId.value).toBeNull();
    expect(w.activeSpaceId.value).toBeNull();
    expect(w.serverRole.value).toBeNull();
    expect(w.mucServiceJid.value).toBeNull();
    expect(w.hasLoadedStructure.value).toBe(false);
    expect(w.isLoadingStructure.value).toBe(false);
    expect(w.editChannelForm.value.name).toBe("");
    expect(w.createChannelForm.value.name).toBe("");
    expect(w.canManageCommunity.value).toBe(false);
    expect(w.canManageMembers.value).toBe(false);
    const fresh = w.loadStructure();
    await settle();
    expect(w.currentChannel.value?.jid).toBe("general@muc.b.test");
    expect(w.memberLoadState.value).toBe("loading");
    expect(w.members.value).toEqual([]);
    expect(w.canManageMembers.value).toBe(false);
    oldMembers.resolve([owner]);
    oldBots.resolve([bot]);
    await stale;
    await settle();
    expect(w.members.value).toEqual([]);
    expect(w.roomBots.value).toEqual([]);
    expect(w.canManageMembers.value).toBe(false);
    if (outcome === "failure") members.reject(new Error("replacement roster unavailable"));
    else members.resolve([ALICE]);
    bots.resolve([]);
    await fresh;
    expect(w.members.value).toEqual(outcome === "failure" ? [] : [ALICE]);
    expect(w.memberLoadState.value).toBe(outcome === "failure" ? "unavailable" : "ready");
    expect(w.canManageMembers.value).toBe(false);
    expect(w.roomBots.value).toEqual([]);
  });

  test.each(["edit", "move", "create"])("delayed %s completion cannot mutate a replacement client", async (operation) => {
    await withFakeDomParser(async () => {
      const pending = deferred<string>();
      const send = mock((xml: string) => xml.includes('type="get"')
        ? Promise.resolve('<iq type="result"/>') : pending.promise);
      const agent = { send_raw_iq: send, join_room: async () => {}, leave_room: async () => {} };
      const old = Object.assign(makeClient({ discoverTopology: async () => topology("a.test") }), {
        send_raw_iq: send, agent,
      });
      const xmppClient = ref<BrowserXmppClient | null>(old);
      const actionError = ref("");
      const w = useWaddleDirectory(xmppClient, ref({ jid: ALICE.jid, username: "alice" } as import("../src/lib/server-auth").WaddleSession), String, actionError, () => {});
      await w.loadStructure();
      w.editChannelForm.value.name = "Old edit";
      w.createChannelForm.value.intent = "muc";
      w.createChannelForm.value.name = "Old creation";
      const completion = operation === "edit" ? w.updateChannel()
        : operation === "move" ? w.moveChannelToSpace("general", "old-target") : w.createChannel();
      await settle();
      expect(send).toHaveBeenCalled();
      expect(w.isSubmitting.value).toBe(true);
      const discoverFresh = mock(async () => topology("b.test"));
      xmppClient.value = makeClient({ discoverTopology: discoverFresh });
      expect(w.isSubmitting.value).toBe(false);
      await w.loadStructure();
      w.editChannelForm.value.name = "New edit";
      actionError.value = "New client status";
      // Simulate another operation's busy state; stale finally must not clear it.
      w.isSubmitting.value = true;
      pending.resolve('<iq type="result"/>');
      expect(await completion).toBe(operation === "create" ? undefined : false);
      expect(w.currentChannel.value?.jid).toBe("general@muc.b.test");
      expect(w.currentChannel.value?.name).toBe("b.test");
      expect(w.currentChannel.value?.spaceId).toBe("space");
      expect(w.editChannelForm.value.name).toBe("New edit");
      expect(actionError.value).toBe("New client status");
      expect(w.isSubmitting.value).toBe(true);
      expect(discoverFresh).toHaveBeenCalledTimes(1);
    });
  });

  test("old structure member completion cannot reset replacement forms or return its selection", async () => {
    const oldMembers = deferred<MemberSummary[]>();
    const { w, xmppClient } = makeWaddles(makeClient({
      discoverTopology: async () => topology("a.test"),
      listRoomMembers: () => oldMembers.promise,
    }));
    const stale = w.loadStructure();
    await settle();
    expect(w.memberLoadState.value).toBe("loading");
    xmppClient.value = makeClient({ discoverTopology: async () => topology("b.test") });
    await w.loadStructure();
    w.createChannelForm.value.name = "New draft";
    oldMembers.resolve([owner]);
    expect(await stale).toBeNull();
    expect(w.createChannelForm.value.name).toBe("New draft");
    expect(w.members.value).toEqual([]);
    expect(w.currentChannel.value?.jid).toBe("general@muc.b.test");
  });

  test("replacement through null invalidates delayed topology and clears loading state", async () => {
    const discovery = deferred<unknown>();
    const { w, xmppClient } = makeWaddles(makeClient({ discoverTopology: () => discovery.promise }));
    const stale = w.loadStructure();
    expect(w.isLoadingStructure.value).toBe(true);
    xmppClient.value = null;
    expect(w.isLoadingStructure.value).toBe(false);
    xmppClient.value = makeClient({ discoverTopology: async () => topology("b.test") });
    await w.loadStructure();
    discovery.resolve(topology("a.test"));
    await stale;
    expect(w.currentChannel.value?.jid).toBe("general@muc.b.test");
    expect(w.serverRole.value).toBeNull();
    expect(w.mucServiceJid.value).toBe("muc.b.test");
    expect(w.hasLoadedStructure.value).toBe(true);
    expect(w.isLoadingStructure.value).toBe(false);
  });
});
