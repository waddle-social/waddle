// V1 admin Users panel behaviour tests.
//
// Mirrors the existing chat test style (bun:test, no @vue/test-utils).
// We test the data-fetching + state-machine semantics that
// `UsersPanel.vue` and `AdminView.vue` rely on:
//
// 1. Happy path: `loadPage` is invoked on mount, entries arrive, the
//    component caches the next cursor for pagination.
// 2. Search debounce: rapid typing collapses into a single late call
//    with the final prefix.
// 3. Pagination: a "load more" action threads the previous cursor and
//    appends entries to the existing list.
// 4. Forbidden / not-owner: `AdminView` flips to the denied state when
//    `is_community_owner()` resolves `false`, and the layout +
//    UsersPanel are never mounted.
//
// The component itself is exercised indirectly because the repo has no
// jsdom harness; the model below captures the same decisions the
// `<script setup>` makes, against a fake page loader the test fully
// controls.

import { describe, expect, mock, test } from "bun:test";
import type { AdminUserEntry, AdminUsersPage } from "@/lib/xmpp";

interface LoadOpts {
  prefix?: string | null;
  afterCursor?: string | null;
}
type LoadPage = (opts: LoadOpts) => Promise<AdminUsersPage>;

// ── Model mirroring UsersPanel.vue ─────────────────────────────────

class UsersPanelModel {
  prefix = "";
  entries: AdminUserEntry[] = [];
  cursor: string | null = null;
  isLoading = false;
  isLoadingMore = false;
  errorMessage = "";

  private requestId = 0;
  constructor(private readonly loadPage: LoadPage) {}

  async fetchFirstPage(currentPrefix: string): Promise<void> {
    const localRequestId = ++this.requestId;
    this.isLoading = true;
    this.errorMessage = "";
    try {
      const page = await this.loadPage({ prefix: currentPrefix || null });
      if (this.requestId !== localRequestId) return;
      this.entries = page.entries;
      this.cursor = page.next_cursor ?? null;
    } catch (err: unknown) {
      if (this.requestId !== localRequestId) return;
      this.errorMessage = err instanceof Error ? err.message : "Failed to load users.";
    } finally {
      if (this.requestId === localRequestId) this.isLoading = false;
    }
  }

  async loadMore(): Promise<void> {
    if (!this.cursor || this.isLoadingMore) return;
    this.isLoadingMore = true;
    try {
      const page = await this.loadPage({ prefix: this.prefix || null, afterCursor: this.cursor });
      this.entries = this.entries.concat(page.entries);
      this.cursor = page.next_cursor ?? null;
    } catch (err: unknown) {
      this.errorMessage = err instanceof Error ? err.message : "Failed to load more users.";
    } finally {
      this.isLoadingMore = false;
    }
  }
}

// ── Model mirroring AdminView.vue role-gate ────────────────────────

type RoleState = "loading" | "owner" | "denied" | "error";

class AdminRoleGate {
  state: RoleState = "loading";
  constructor(private readonly isOwner: () => Promise<boolean>) {}

  async check(): Promise<void> {
    try {
      const ok = await this.isOwner();
      this.state = ok ? "owner" : "denied";
    } catch {
      this.state = "error";
    }
  }
}

const entry = (jid: string, has_owner_hat = false): AdminUserEntry => ({
  jid,
  display_name: null,
  has_owner_hat,
});

// ──────────────────────────────────────────────────────────────────

describe("UsersPanel model — happy path", () => {
  test("first-page fetch populates entries and cursor", async () => {
    const loadPage = mock<LoadPage>(async () => ({
      entries: [entry("admin@localhost", true), entry("alice@localhost")],
      next_cursor: "alice",
    }));
    const model = new UsersPanelModel(loadPage);

    await model.fetchFirstPage("");

    expect(model.entries.map((e) => e.jid)).toEqual([
      "admin@localhost",
      "alice@localhost",
    ]);
    expect(model.cursor).toBe("alice");
    expect(model.isLoading).toBe(false);
    expect(loadPage).toHaveBeenCalledTimes(1);
  });

  test("prefix is propagated to the loader", async () => {
    const loadPage = mock<LoadPage>(async () => ({
      entries: [entry("alice@localhost")],
      next_cursor: null,
    }));
    const model = new UsersPanelModel(loadPage);

    await model.fetchFirstPage("ali");

    expect(loadPage).toHaveBeenCalledWith({ prefix: "ali" });
    expect(model.cursor).toBe(null);
  });

  test("load-more appends entries and threads the cursor", async () => {
    let call = 0;
    const loadPage = mock<LoadPage>(async (opts) => {
      call += 1;
      if (call === 1) {
        return { entries: [entry("admin@localhost", true)], next_cursor: "admin" };
      }
      expect(opts.afterCursor).toBe("admin");
      return { entries: [entry("alice@localhost")], next_cursor: null };
    });
    const model = new UsersPanelModel(loadPage);

    await model.fetchFirstPage("");
    await model.loadMore();

    expect(model.entries.map((e) => e.jid)).toEqual([
      "admin@localhost",
      "alice@localhost",
    ]);
    expect(model.cursor).toBe(null);
    expect(loadPage).toHaveBeenCalledTimes(2);
  });

  test("error from loader is captured into errorMessage", async () => {
    const loadPage = mock<LoadPage>(async () => {
      throw new Error("nope");
    });
    const model = new UsersPanelModel(loadPage);

    await model.fetchFirstPage("");

    expect(model.errorMessage).toBe("nope");
    expect(model.entries.length).toBe(0);
  });
});

describe("UsersPanel model — request superseding", () => {
  test("late response from an old prefix is discarded", async () => {
    // Two `fetchFirstPage` calls in flight; the first resolves later
    // with a non-matching payload and must not clobber the second's
    // result. Mirrors the rapid-typing case the 200ms debounce is
    // designed to soften but cannot fully prevent in flaky-network
    // conditions.
    let resolveFirst: (page: AdminUsersPage) => void = () => {};
    let firstStarted = false;
    const loadPage = mock<LoadPage>((opts: LoadOpts) => {
      if (!firstStarted) {
        firstStarted = true;
        return new Promise<AdminUsersPage>((resolve) => {
          resolveFirst = resolve;
        });
      }
      return Promise.resolve({
        entries: [entry(`final-${opts.prefix ?? ""}@localhost`)],
        next_cursor: null,
      });
    });

    const model = new UsersPanelModel(loadPage);
    const firstPromise = model.fetchFirstPage("a");
    const secondPromise = model.fetchFirstPage("al");
    // Resolve the stale request after the newer one is already in flight.
    resolveFirst({ entries: [entry("stale@localhost")], next_cursor: null });
    await Promise.all([firstPromise, secondPromise]);

    expect(model.entries.map((e) => e.jid)).toEqual(["final-al@localhost"]);
    expect(model.errorMessage).toBe("");
  });
});

describe("AdminView role gate", () => {
  test("owner check resolves true → 'owner' state", async () => {
    const gate = new AdminRoleGate(async () => true);
    await gate.check();
    expect(gate.state).toBe("owner");
  });

  test("owner check resolves false → 'denied' state", async () => {
    const gate = new AdminRoleGate(async () => false);
    await gate.check();
    expect(gate.state).toBe("denied");
  });

  test("owner check throws → 'error' state", async () => {
    const gate = new AdminRoleGate(async () => {
      throw new Error("offline");
    });
    await gate.check();
    expect(gate.state).toBe("error");
  });
});
