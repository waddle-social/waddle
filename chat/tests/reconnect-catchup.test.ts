import { describe, expect, test } from "bun:test";
import { ReconnectCatchup } from "../src/lib/xmpp/reconnect-catchup";

// The ReconnectCatchup helper underpins Slice A of the multi-device sync fix.
// It tracks a per-conversation "last-seen" cursor and distinguishes the first
// session:started (initial login) from subsequent ones (resumed after a
// socket drop). Mobile Safari aggressively suspends WebSockets when the tab
// backgrounds; without reconnect catch-up, messages sent during the suspend
// are lost to that session forever.

describe("ReconnectCatchup", () => {
  test("first onSessionStarted returns no keys to catch up on", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");

    expect(c.onSessionStarted()).toEqual([]);
  });

  test("subsequent onSessionStarted returns all tracked conversations with their kind", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z", "dm-arch-1");
    c.recordRoomSeen("general@muc.example.com", "2024-01-02T00:00:00Z", "room-arch-1");

    // First session:started = initial login; skip catch-up.
    c.onSessionStarted();
    // Second session:started = resume after drop; catch up everywhere.
    const entries = c.onSessionStarted();

    expect(entries).toHaveLength(2);
    expect(entries).toContainEqual({
      kind: "dm",
      key: "bob@example.com",
      after: "dm-arch-1",
    });
    expect(entries).toContainEqual({
      kind: "room",
      key: "general@muc.example.com",
      after: "room-arch-1",
    });
  });

  test("getDmLastSeen returns the most recent timestamp recorded for a peer", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z");

    expect(c.getDmLastSeen("bob@example.com")).toBe("2024-01-02T00:00:00.000Z");
  });

  test("subsequent onSessionStarted falls back to timestamp when no archive id is known", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "dm", key: "bob@example.com", since: "2024-01-01T00:00:00.000Z" },
    ]);
  });

  test("timestamp fallback carries seen ids for exact-boundary dedupe", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z", undefined, ["dm-1"]);
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z", undefined, ["dm-2"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      {
        kind: "dm",
        key: "bob@example.com",
        since: "2024-01-01T00:00:00.000Z",
        seenIds: ["dm-1", "dm-2"],
      },
    ]);
  });

  test("timestamp fallback normalizes equivalent RFC3339 instants before merging seen ids", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z", undefined, ["dm-1"]);
    c.recordDmSeen("bob@example.com", "2024-01-01T01:00:00+01:00", undefined, ["dm-2"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      {
        kind: "dm",
        key: "bob@example.com",
        since: "2024-01-01T00:00:00.000Z",
        seenIds: ["dm-1", "dm-2"],
      },
    ]);
  });

  test("recordSeen fills archive id for the current timestamp without rewinding time", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-2");

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "dm", key: "bob@example.com", after: "dm-arch-2" },
    ]);
  });

  test("recordSeen advances archive id for repeated archived messages at the current timestamp", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-1");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-2");

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "dm", key: "bob@example.com", after: "dm-arch-2" },
    ]);
  });

  test("newer live timestamps do not discard the last authoritative archive id", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-2");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:01Z", undefined, ["dm-live-3"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      {
        kind: "dm",
        key: "bob@example.com",
        after: "dm-arch-2",
        seenIds: ["dm-live-3"],
      },
    ]);
  });

  test("same-timestamp live ids remain available when catch-up can use an archive cursor", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-2");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00.000+00:00", undefined, ["dm-live-same-time"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      {
        kind: "dm",
        key: "bob@example.com",
        after: "dm-arch-2",
        seenIds: ["dm-live-same-time"],
      },
    ]);
  });

  test("recordSeen ignores out-of-order (older) timestamps", () => {
    // Carbons / MAM results can arrive out of order. We must not rewind the
    // cursor, or the next catch-up will re-pull already-applied messages
    // and drown the UI in duplicates.
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z");
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");

    expect(c.getDmLastSeen("bob@example.com")).toBe("2024-01-02T00:00:00.000Z");
  });

  test("DM and room namespaces are independent — same JID in both kinds does not collide", () => {
    // Defensive: a bare JID `foo@bar` could theoretically appear as both a
    // DM peer and a room. Their cursors must not interfere.
    const c = new ReconnectCatchup();
    c.recordDmSeen("foo@bar", "2024-01-01T00:00:00Z");
    c.recordRoomSeen("foo@bar", "2024-02-02T00:00:00Z");

    expect(c.getDmLastSeen("foo@bar")).toBe("2024-01-01T00:00:00.000Z");
    expect(c.getRoomLastSeen("foo@bar")).toBe("2024-02-02T00:00:00.000Z");
  });

  test("getDmLastSeen returns undefined for untracked peers", () => {
    const c = new ReconnectCatchup();
    expect(c.getDmLastSeen("nobody@example.com")).toBeUndefined();
  });

  test("reset clears session-started state so the next session:started is treated as initial", () => {
    // Called on intentional disconnect so a subsequent login starts fresh.
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");
    c.onSessionStarted();
    c.reset();

    expect(c.onSessionStarted()).toEqual([]);
  });

  test("reset clears tracked cursors", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");
    c.recordRoomSeen("general@muc.example.com", "2024-01-01T00:00:00Z");
    c.reset();

    expect(c.getDmLastSeen("bob@example.com")).toBeUndefined();
    expect(c.getRoomLastSeen("general@muc.example.com")).toBeUndefined();
  });
});
