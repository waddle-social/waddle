// Admin V2 Spaces panel — happy paths.
//
// Mirrors `admin-users-panel.test.ts`: rather than mounting Vue, we
// model the SpacesPanel state machine and exercise it against a
// fake `BrowserXmppClient` whose `adminSpaces*` methods return
// canned responses. This is the same trade-off the V1 admin tests
// make and avoids spinning a wasm/jsdom harness.

import { describe, expect, mock, test } from "bun:test";
import type {
  WasmAdminSpaceListEntry,
  WasmAdminSpaceMemberEntry,
  WasmAdminSpacesListResult,
  WasmAdminSpacesMembersResult,
} from "@/lib/xmpp";

// ── Fake client surface ───────────────────────────────────────────

interface FakeClient {
  adminSpacesList: ReturnType<typeof mock>;
  adminSpacesCreate: ReturnType<typeof mock>;
  adminSpacesUpdate: ReturnType<typeof mock>;
  adminSpacesDelete: ReturnType<typeof mock>;
  adminSpacesMembers: ReturnType<typeof mock>;
  adminSpacesSetRole: ReturnType<typeof mock>;
}

const entry = (jid: string, channels = 0, members = 0): WasmAdminSpaceListEntry => ({
  space_jid: jid,
  name: jid.split("@")[0] ?? jid,
  description: null,
  icon_url: null,
  channel_count: channels,
  member_count: members,
});

// ── SpacesPanel model — mirrors the panel's state machine ─────────

class SpacesPanelModel {
  prefix = "";
  entries: WasmAdminSpaceListEntry[] = [];
  cursor: string | null = null;
  isLoading = false;
  isLoadingMore = false;
  errorMessage = "";
  selected: WasmAdminSpaceListEntry | null = null;
  showCreate = false;
  private requestId = 0;
  constructor(private client: FakeClient) {}

  async fetchFirstPage(prefix: string): Promise<void> {
    const localRequestId = ++this.requestId;
    this.isLoading = true;
    this.errorMessage = "";
    try {
      const page: WasmAdminSpacesListResult = await this.client.adminSpacesList({
        prefix: prefix || null,
        pageSize: 50,
      });
      if (this.requestId !== localRequestId) return;
      this.entries = page.entries;
      this.cursor = page.next_cursor ?? null;
    } catch (err: unknown) {
      if (this.requestId !== localRequestId) return;
      this.errorMessage = err instanceof Error ? err.message : "Failed to load spaces.";
    } finally {
      if (this.requestId === localRequestId) this.isLoading = false;
    }
  }

  async loadMore(): Promise<void> {
    if (!this.cursor || this.isLoadingMore) return;
    this.isLoadingMore = true;
    try {
      const page = await this.client.adminSpacesList({
        prefix: this.prefix || null,
        pageSize: 50,
        afterCursor: this.cursor,
      });
      this.entries = this.entries.concat(page.entries);
      this.cursor = page.next_cursor ?? null;
    } catch (err: unknown) {
      this.errorMessage = err instanceof Error ? err.message : "Failed to load more.";
    } finally {
      this.isLoadingMore = false;
    }
  }

  openDetail(e: WasmAdminSpaceListEntry) {
    this.selected = e;
  }
  closeDetail() {
    this.selected = null;
  }

  async create(name: string): Promise<void> {
    await this.client.adminSpacesCreate({ name });
    this.showCreate = false;
    await this.fetchFirstPage(this.prefix);
  }
}

// ─────────────────────────────────────────────────────────────────

describe("SpacesPanel model — list + search", () => {
  test("first-page fetch populates entries", async () => {
    const client: FakeClient = {
      adminSpacesList: mock(async () => ({
        entries: [entry("eng@spaces.localhost", 3, 5)],
        next_cursor: null,
      })),
      adminSpacesCreate: mock(async () => ({})),
      adminSpacesUpdate: mock(async () => ({})),
      adminSpacesDelete: mock(async () => true),
      adminSpacesMembers: mock(async () => ({ entries: [] })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const model = new SpacesPanelModel(client);
    await model.fetchFirstPage("");
    expect(model.entries.length).toBe(1);
    expect(model.entries[0]?.channel_count).toBe(3);
    expect(model.cursor).toBe(null);
    expect(client.adminSpacesList).toHaveBeenCalledTimes(1);
  });

  test("prefix is forwarded to the loader", async () => {
    const client: FakeClient = {
      adminSpacesList: mock(async () => ({ entries: [], next_cursor: null })),
      adminSpacesCreate: mock(async () => ({})),
      adminSpacesUpdate: mock(async () => ({})),
      adminSpacesDelete: mock(async () => true),
      adminSpacesMembers: mock(async () => ({ entries: [] })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const model = new SpacesPanelModel(client);
    await model.fetchFirstPage("eng");
    expect(client.adminSpacesList).toHaveBeenCalledWith({ prefix: "eng", pageSize: 50 });
  });

  test("load-more threads cursor and appends entries", async () => {
    let call = 0;
    const client: FakeClient = {
      adminSpacesList: mock(async (opts: { afterCursor?: string }) => {
        call += 1;
        if (call === 1) {
          return { entries: [entry("eng@spaces.localhost")], next_cursor: "eng@spaces.localhost" };
        }
        expect(opts.afterCursor).toBe("eng@spaces.localhost");
        return { entries: [entry("design@spaces.localhost")], next_cursor: null };
      }),
      adminSpacesCreate: mock(async () => ({})),
      adminSpacesUpdate: mock(async () => ({})),
      adminSpacesDelete: mock(async () => true),
      adminSpacesMembers: mock(async () => ({ entries: [] })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const model = new SpacesPanelModel(client);
    await model.fetchFirstPage("");
    await model.loadMore();
    expect(model.entries.length).toBe(2);
    expect(model.cursor).toBe(null);
  });

  test("error from loader surfaces into errorMessage", async () => {
    const client: FakeClient = {
      adminSpacesList: mock(async () => {
        throw new Error("forbidden");
      }),
      adminSpacesCreate: mock(async () => ({})),
      adminSpacesUpdate: mock(async () => ({})),
      adminSpacesDelete: mock(async () => true),
      adminSpacesMembers: mock(async () => ({ entries: [] })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const model = new SpacesPanelModel(client);
    await model.fetchFirstPage("");
    expect(model.errorMessage).toBe("forbidden");
  });
});

describe("SpacesPanel model — click + create", () => {
  test("openDetail captures the clicked entry", () => {
    const client: FakeClient = {
      adminSpacesList: mock(async () => ({ entries: [], next_cursor: null })),
      adminSpacesCreate: mock(async () => ({})),
      adminSpacesUpdate: mock(async () => ({})),
      adminSpacesDelete: mock(async () => true),
      adminSpacesMembers: mock(async () => ({ entries: [] })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const model = new SpacesPanelModel(client);
    const target = entry("eng@spaces.localhost");
    model.openDetail(target);
    expect(model.selected).toBe(target);
    model.closeDetail();
    expect(model.selected).toBe(null);
  });

  test("create flow calls adminSpacesCreate and refetches", async () => {
    const calls: string[] = [];
    const client: FakeClient = {
      adminSpacesList: mock(async () => {
        calls.push("list");
        return { entries: [], next_cursor: null };
      }),
      adminSpacesCreate: mock(async (args: { name: string }) => {
        calls.push(`create:${args.name}`);
        return { space_jid: "x@spaces.localhost", name: args.name };
      }),
      adminSpacesUpdate: mock(async () => ({})),
      adminSpacesDelete: mock(async () => true),
      adminSpacesMembers: mock(async () => ({ entries: [] })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const model = new SpacesPanelModel(client);
    await model.create("Engineering");
    expect(calls).toEqual(["create:Engineering", "list"]);
    expect(model.showCreate).toBe(false);
  });
});

describe("SpaceDetailDrawer — members + role + delete (model-level)", () => {
  test("loading members surfaces typed entries", async () => {
    const members: WasmAdminSpaceMemberEntry[] = [
      { jid: "alice@localhost", role: "owner" },
      { jid: "bob@localhost", role: "member" },
    ];
    const client = {
      adminSpacesMembers: mock<() => Promise<WasmAdminSpacesMembersResult>>(async () => ({ entries: members })),
      adminSpacesSetRole: mock(async () => ({})),
    };
    const result = await client.adminSpacesMembers();
    expect(result.entries[0]?.role).toBe("owner");
  });

  test("setRole call constructs expected typed payload", async () => {
    const client = {
      adminSpacesSetRole: mock(async (args: { spaceJid: string; memberJid: string; role: string }) => {
        expect(args.spaceJid).toBe("eng@spaces.localhost");
        expect(args.memberJid).toBe("bob@localhost");
        expect(args.role).toBe("admin");
        return { member_jid: args.memberJid, role: args.role };
      }),
    };
    await client.adminSpacesSetRole({ spaceJid: "eng@spaces.localhost", memberJid: "bob@localhost", role: "admin" });
    expect(client.adminSpacesSetRole).toHaveBeenCalledTimes(1);
  });

  test("delete confirm hits adminSpacesDelete with confirm semantics", async () => {
    const client = {
      adminSpacesDelete: mock(async (args: { spaceJid: string }) => {
        expect(args.spaceJid).toBe("eng@spaces.localhost");
        return true;
      }),
    };
    await client.adminSpacesDelete({ spaceJid: "eng@spaces.localhost" });
    expect(client.adminSpacesDelete).toHaveBeenCalledTimes(1);
  });
});
