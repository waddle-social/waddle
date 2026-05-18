// Admin V2 Channels panel — happy paths.
//
// Same shape as admin-spaces-panel.test.ts: model the panel's state
// machine and exercise it against a fake `BrowserXmppClient`.

import { describe, expect, mock, test } from "bun:test";
import type {
  WasmAdminChannelListEntry,
  WasmAdminChannelsListResult,
} from "@/lib/xmpp";

interface FakeClient {
  adminChannelsList: ReturnType<typeof mock>;
  adminChannelsCreate: ReturnType<typeof mock>;
  adminChannelsDelete: ReturnType<typeof mock>;
  adminChannelsAffiliations: ReturnType<typeof mock>;
  adminChannelsOccupants: ReturnType<typeof mock>;
  adminChannelsKick: ReturnType<typeof mock>;
  adminChannelsSetAffiliation: ReturnType<typeof mock>;
}

const entry = (
  jid: string,
  opts: Partial<WasmAdminChannelListEntry> = {},
): WasmAdminChannelListEntry => ({
  channel_jid: jid,
  name: jid.split("@")[0] ?? jid,
  topic: null,
  is_public: true,
  members_only: false,
  occupant_count: 0,
  owner_count: 0,
  admin_count: 0,
  member_count: 0,
  outcast_count: 0,
  ...opts,
});

class ChannelsPanelModel {
  prefix = "";
  spaceFilter: string | null = null;
  entries: WasmAdminChannelListEntry[] = [];
  cursor: string | null = null;
  isLoading = false;
  errorMessage = "";
  selected: WasmAdminChannelListEntry | null = null;
  private requestId = 0;
  constructor(private client: FakeClient) {}

  async fetchFirstPage(prefix: string, spaceFilter: string | null): Promise<void> {
    const localRequestId = ++this.requestId;
    this.isLoading = true;
    this.errorMessage = "";
    try {
      const page: WasmAdminChannelsListResult = await this.client.adminChannelsList({
        spaceJid: spaceFilter ?? null,
        prefix: prefix || null,
        pageSize: 50,
      });
      if (this.requestId !== localRequestId) return;
      this.entries = page.entries;
      this.cursor = page.next_cursor ?? null;
    } catch (err: unknown) {
      if (this.requestId !== localRequestId) return;
      this.errorMessage = err instanceof Error ? err.message : "Failed to load channels.";
    } finally {
      if (this.requestId === localRequestId) this.isLoading = false;
    }
  }

  async create(name: string): Promise<void> {
    await this.client.adminChannelsCreate({ name, isPublic: true });
    await this.fetchFirstPage(this.prefix, this.spaceFilter);
  }

  async deleteSelected(): Promise<void> {
    if (!this.selected) return;
    await this.client.adminChannelsDelete({ channelJid: this.selected.channel_jid });
    this.selected = null;
  }

  async kickFirstOccupant(): Promise<void> {
    if (!this.selected) return;
    const page = await this.client.adminChannelsOccupants({
      channelJid: this.selected.channel_jid,
    });
    const occupants = page.entries as Array<{ real_jid: string }>;
    if (occupants.length === 0) return;
    const bareJid = occupants[0]!.real_jid.split("/")[0] ?? occupants[0]!.real_jid;
    await this.client.adminChannelsKick({
      channelJid: this.selected.channel_jid,
      occupantJid: bareJid,
    });
  }
}

// ─────────────────────────────────────────────────────────────────

describe("ChannelsPanel model — list + filter + search", () => {
  test("first-page fetch populates entries", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({
        entries: [entry("general@muc.localhost", { occupant_count: 5 })],
        next_cursor: null,
      })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    await model.fetchFirstPage("", null);
    expect(model.entries[0]?.occupant_count).toBe(5);
  });

  test("space filter is forwarded to the loader", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    await model.fetchFirstPage("", "eng@spaces.localhost");
    expect(client.adminChannelsList).toHaveBeenCalledWith({
      spaceJid: "eng@spaces.localhost",
      prefix: null,
      pageSize: 50,
    });
  });

  test("create call defaults to isPublic=true (spec)", async () => {
    let capturedArgs: { name?: string; isPublic?: boolean } | null = null;
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async (args: { name: string; isPublic?: boolean }) => {
        capturedArgs = args;
        return { channel_jid: "x@muc.localhost", name: args.name, is_public: true };
      }),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    await model.create("general");
    expect(capturedArgs).not.toBeNull();
    expect(capturedArgs!.isPublic).toBe(true);
  });
});

describe("ChannelsPanel model — destructive actions", () => {
  test("delete clears selected and calls adminChannelsDelete", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsDelete: mock(async (args: { channelJid: string }) => {
        expect(args.channelJid).toBe("general@muc.localhost");
        return true;
      }),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    model.selected = entry("general@muc.localhost");
    await model.deleteSelected();
    expect(model.selected).toBe(null);
    expect(client.adminChannelsDelete).toHaveBeenCalledTimes(1);
  });

  test("kick forwards the bare JID (resource stripped)", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({
        entries: [{ nick: "alice", real_jid: "alice@localhost/web", role: "participant", affiliation: "member" }],
      })),
      adminChannelsKick: mock(async (args: { channelJid: string; occupantJid: string }) => {
        expect(args.channelJid).toBe("general@muc.localhost");
        expect(args.occupantJid).toBe("alice@localhost");
        return { occupant_jid: args.occupantJid };
      }),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    model.selected = entry("general@muc.localhost");
    await model.kickFirstOccupant();
    expect(client.adminChannelsKick).toHaveBeenCalledTimes(1);
  });
});
