// Admin V2 Channels panel — happy paths.
//
// Same shape as admin-spaces-panel.test.ts: model the panel's state
// machine and exercise it against a fake `BrowserXmppClient`.

import { describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { requireMembershipForUnlistedChannel } from "@/components/admin/channelAdmissionPolicy";
import type {
  WasmAdminChannelListEntry,
  WasmAdminChannelsListResult,
} from "@/lib/xmpp";

interface FakeClient {
  adminChannelsList: ReturnType<typeof mock>;
  adminChannelsCreate: ReturnType<typeof mock>;
  adminChannelsUpdate?: ReturnType<typeof mock>;
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
  channel_type: "text",
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

  async create(name: string, isPublic = true, membersOnly = false): Promise<void> {
    await this.client.adminChannelsCreate({ name, isPublic, membersOnly });
    await this.fetchFirstPage(this.prefix, this.spaceFilter);
  }

  async deleteSelected(): Promise<void> {
    if (!this.selected) return;
    await this.client.adminChannelsDelete({ channelJid: this.selected.channel_jid });
    this.selected = null;
  }

  async updateSelectedVisibility(isPublic: boolean): Promise<void> {
    if (!this.selected || !this.client.adminChannelsUpdate) return;
    const policy = requireMembershipForUnlistedChannel({
      isPublic,
      membersOnly: this.selected.members_only,
    });
    await this.updateSelectedConfig(policy.isPublic, policy.membersOnly);
  }

  async updateSelectedConfig(isPublic: boolean, membersOnly: boolean): Promise<void> {
    if (!this.selected || !this.client.adminChannelsUpdate) return;
    await this.client.adminChannelsUpdate({
      channelJid: this.selected.channel_jid,
      name: this.selected.name,
      topic: this.selected.topic ?? null,
      isPublic,
      membersOnly,
    });
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
    await this.client.adminChannelsOccupants({
      channelJid: this.selected.channel_jid,
    });
    await this.client.adminChannelsAffiliations({
      channelJid: this.selected.channel_jid,
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

  test("create call defaults to listed and open admission", async () => {
    let capturedArgs: { name?: string; isPublic?: boolean; membersOnly?: boolean } | null = null;
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async (args: { name: string; isPublic?: boolean; membersOnly?: boolean }) => {
        capturedArgs = args;
        return { channel_jid: "x@muc.localhost", name: args.name, is_public: true, members_only: false };
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
    expect(capturedArgs!.membersOnly).toBe(false);
  });

  test("create call can request hidden members-only rooms", async () => {
    let capturedArgs: { name?: string; isPublic?: boolean; membersOnly?: boolean } | null = null;
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async (args: { name: string; isPublic?: boolean; membersOnly?: boolean }) => {
        capturedArgs = args;
        return { channel_jid: "x@muc.localhost", name: args.name, is_public: false, members_only: true };
      }),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    await model.create("ops", false, true);
    expect(capturedArgs).not.toBeNull();
    expect(capturedArgs!.isPublic).toBe(false);
    expect(capturedArgs!.membersOnly).toBe(true);
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

  test("kick forwards the bare JID and refreshes occupants plus affiliations", async () => {
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
    expect(client.adminChannelsOccupants).toHaveBeenCalledTimes(2);
    expect(client.adminChannelsAffiliations).toHaveBeenCalledTimes(1);
  });

  test("private visibility update also requests members-only admission", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsUpdate: mock(async (args: { channelJid: string; isPublic: boolean; membersOnly: boolean }) => {
        expect(args.channelJid).toBe("general@muc.localhost");
        expect(args.isPublic).toBe(false);
        expect(args.membersOnly).toBe(true);
        return { channel_jid: args.channelJid, name: "general", is_public: false, members_only: true };
      }),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    model.selected = entry("general@muc.localhost", { name: "general", topic: null });
    await model.updateSelectedVisibility(false);
    expect(client.adminChannelsUpdate).toHaveBeenCalledTimes(1);
  });

  test("unchanged visibility update preserves existing members-only policy", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsUpdate: mock(async (args: { channelJid: string; isPublic: boolean; membersOnly: boolean }) => {
        expect(args.channelJid).toBe("public-members@muc.localhost");
        expect(args.isPublic).toBe(true);
        expect(args.membersOnly).toBe(true);
        return { channel_jid: args.channelJid, name: "public-members", is_public: true, members_only: true };
      }),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    model.selected = entry("public-members@muc.localhost", {
      name: "public-members",
      is_public: true,
      members_only: true,
    });
    await model.updateSelectedVisibility(true);
    expect(client.adminChannelsUpdate).toHaveBeenCalledTimes(1);
  });

  test("config update can preserve hidden-open channel semantics", async () => {
    const client: FakeClient = {
      adminChannelsList: mock(async () => ({ entries: [], next_cursor: null })),
      adminChannelsCreate: mock(async () => ({})),
      adminChannelsUpdate: mock(async (args: { channelJid: string; isPublic: boolean; membersOnly: boolean }) => {
        expect(args.channelJid).toBe("hidden-open@muc.localhost");
        expect(args.isPublic).toBe(false);
        expect(args.membersOnly).toBe(false);
        return { channel_jid: args.channelJid, name: "hidden-open", is_public: false, members_only: false };
      }),
      adminChannelsDelete: mock(async () => true),
      adminChannelsAffiliations: mock(async () => ({ entries: [] })),
      adminChannelsOccupants: mock(async () => ({ entries: [] })),
      adminChannelsKick: mock(async () => ({})),
      adminChannelsSetAffiliation: mock(async () => ({})),
    };
    const model = new ChannelsPanelModel(client);
    model.selected = entry("hidden-open@muc.localhost", {
      name: "hidden-open",
      is_public: false,
      members_only: false,
    });
    await model.updateSelectedConfig(false, false);
    expect(client.adminChannelsUpdate).toHaveBeenCalledTimes(1);
  });
});

describe("ChannelsPanel source wiring — XEP visibility vs admission", () => {
  const readSource = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

  test("shared policy requires explicit membership when a channel becomes unlisted", () => {
    expect(requireMembershipForUnlistedChannel({
      isPublic: false,
      membersOnly: false,
    })).toEqual({
      isPublic: false,
      membersOnly: true,
    });
    expect(requireMembershipForUnlistedChannel({
      isPublic: true,
      membersOnly: false,
    })).toEqual({
      isPublic: true,
      membersOnly: false,
    });
    expect(requireMembershipForUnlistedChannel({
      isPublic: false,
      membersOnly: true,
    })).toEqual({
      isPublic: false,
      membersOnly: true,
    });
  });

  test("create dialog exposes separate listed and members-only controls", () => {
    const source = readSource("../src/components/admin/ChannelCreateDialog.vue");
    expect(source).toContain("const membersOnly = ref(false)");
    expect(source).toContain("handlePublicToggleChange");
    expect(source).toContain("requireMembershipForUnlistedChannel");
    expect(source).toContain("Listed in discovery");
    expect(source).toContain("Require explicit membership");
    expect(source).toContain("membersOnly: membersOnly.value");
  });

  test("detail drawer save sends both independent room settings", () => {
    const source = readSource("../src/components/admin/ChannelDetailDrawer.vue");
    expect(source).toContain("const editMembersOnly = ref(props.channel.members_only)");
    expect(source).toContain("editMembersOnly.value = policy.membersOnly");
    expect(source).toContain('v-model="editIsPublic" type="checkbox" @change="handlePublicToggleChange"');
    expect(source).toContain("membersOnly: editMembersOnly.value");
    expect(source).toContain("Listed in discovery");
    expect(source).toContain("Require explicit membership");
    expect(source).toMatch(
      /adminChannelsKick[\s\S]*await loadOccupants\(\);[\s\S]*await loadAffiliations\(\);/,
    );
  });

  test("channel list labels hidden and members-only independently", () => {
    const source = readSource("../src/components/admin/ChannelsPanel.vue");
    expect(source).toContain("entry.members_only");
    expect(source).toContain(">Hidden<");
    expect(source).toContain(">Members-only<");
    expect(source).not.toContain(">Private<");
  });

  test("browser XMPP wrapper maps membersOnly into the WASM payload", () => {
    const source = readSource("../src/lib/xmpp/client.ts");
    expect(source).toContain("membersOnly?: boolean | null");
    expect(source).toContain("members_only: opts.membersOnly ?? null");
  });
});
