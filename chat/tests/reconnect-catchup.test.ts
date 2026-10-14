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
      scope: "account",
      key: "bob@example.com",
      after: "dm-arch-1",
      since: "2024-01-01T00:00:00.000Z",
    });
    expect(entries).toContainEqual({
      kind: "room",
      key: "general@muc.example.com",
      after: "room-arch-1",
      since: "2024-01-02T00:00:00.000Z",
    });
  });

  test("getDmLastSeen returns the most recent timestamp recorded for a peer", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z");

    expect(c.getDmLastSeen("bob@example.com")).toBe("2024-01-02T00:00:00.000Z");
  });

  test("MUC occupant scope uses a canonical full-JID key", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen(
      " Room@Rooms.Example/Alice ",
      "2024-01-02T00:00:00Z",
      "mam-occupant-1",
      [],
      "muc-occupant",
    );

    expect(c.getDmScope("room@rooms.example/Alice")).toBe("muc-occupant");
    expect(c.getDmLastSeen("ROOM@ROOMS.EXAMPLE/Alice")).toBe("2024-01-02T00:00:00.000Z");
    expect(c.onSessionStarted()).toEqual([]);
    expect(c.onSessionStarted()).toEqual([{
      kind: "dm",
      key: "room@rooms.example/Alice",
      scope: "muc-occupant",
      after: "mam-occupant-1",
      since: "2024-01-02T00:00:00.000Z",
    }]);
  });

  test("subsequent onSessionStarted falls back to timestamp when no archive id is known", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "dm", scope: "account", key: "bob@example.com", since: "2024-01-01T00:00:00.000Z" },
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
        scope: "account",
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
        scope: "account",
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
      { kind: "dm", scope: "account", key: "bob@example.com", after: "dm-arch-2", since: "2024-01-02T00:00:00.000Z" },
    ]);
  });

  test("recordSeen advances archive id for repeated archived messages at the current timestamp", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-1");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-2");

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "dm", scope: "account", key: "bob@example.com", after: "dm-arch-2", since: "2024-01-02T00:00:00.000Z" },
    ]);
  });

  test("newer live timestamps do not discard the last authoritative archive id", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z", "dm-arch-2", ["dm-arch-message"]);
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:01Z", undefined, ["dm-live-1"]);
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:02Z", undefined, ["dm-live-2"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      {
        kind: "dm",
        scope: "account",
        key: "bob@example.com",
        after: "dm-arch-2",
        since: "2024-01-02T00:00:00.000Z",
        seenIds: ["dm-arch-message", "dm-live-1", "dm-live-2"],
      },
    ]);
  });

  test("archive records behind the live clock still advance the archive cursor", () => {
    // Live stanzas carry client receipt time, which runs ahead of the
    // archive's server stamp for the same message.
    const c = new ReconnectCatchup();
    c.recordRoomSeen("room@muc.example.com", "2024-01-02T00:00:00Z", "arch-1", ["m1"]);
    c.recordRoomSeen("room@muc.example.com", "2024-01-02T00:00:05Z", undefined, ["m2"]);
    c.recordRoomSeen("room@muc.example.com", "2024-01-02T00:00:04Z", "arch-2", ["m2"]);
    c.recordRoomSeen("room@muc.example.com", "2024-01-02T00:00:01Z", "arch-older", ["m0"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      {
        kind: "room",
        key: "room@muc.example.com",
        after: "arch-2",
        since: "2024-01-02T00:00:04.000Z",
        // Live ids keep accumulating; ids behind the cursor are harmless.
        seenIds: ["m2", "m1"],
      },
    ]);
    expect(c.getRoomLastSeen("room@muc.example.com")).toBe("2024-01-02T00:00:05.000Z");
  });

  test("a newer live stamp never rewinds the archive cursor (another tab's older shard)", () => {
    const c = new ReconnectCatchup();
    c.recordRoomSeen("room@muc.example.com", "2024-01-02T00:00:10Z", "arch-new");
    // Another tab: later receipt time, but its archive cursor is older.
    (c as unknown as {
      absorbRemoteSnapshot: (snapshot: unknown) => void;
    }).absorbRemoteSnapshot({
      dmLastSeen: [],
      roomLastSeen: [["room@muc.example.com", {
        timestamp: "2024-01-02T00:00:20.000Z",
        archiveId: "arch-old",
        archiveTimestamp: "2024-01-02T00:00:05.000Z",
      }]],
    });

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "room", key: "room@muc.example.com", after: "arch-new", since: "2024-01-02T00:00:10.000Z" },
    ]);
    expect(c.getRoomLastSeen("room@muc.example.com")).toBe("2024-01-02T00:00:20.000Z");
  });

  test("scrolled-back history does not seed a live-only cursor's archive position", () => {
    const c = new ReconnectCatchup();
    c.recordRoomSeen("room@muc.example.com", "2024-01-02T00:00:10Z", undefined, ["live-1"]);
    c.recordRoomSeen("room@muc.example.com", "2024-01-01T00:00:00Z", "arch-ancient", ["old-1"]);

    c.onSessionStarted();

    expect(c.onSessionStarted()).toEqual([
      { kind: "room", key: "room@muc.example.com", since: "2024-01-02T00:00:10.000Z", seenIds: ["live-1"] },
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
        scope: "account",
        key: "bob@example.com",
        after: "dm-arch-2",
        since: "2024-01-02T00:00:00.000Z",
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

  test("catch-up covers only the 20 most recently active conversations", () => {
    const c = new ReconnectCatchup();
    c.onSessionStarted();
    for (let i = 0; i < 25; i += 1) {
      const at = `2024-01-01T00:00:${String(i).padStart(2, "0")}Z`;
      if (i % 2 === 0) c.recordRoomSeen(`room-${i}@muc.example.com`, at);
      else c.recordDmSeen(`peer-${i}@example.com`, at);
    }

    const keys = c.onSessionStarted().map((entry) => entry.key);

    expect(keys).toHaveLength(20);
    expect(keys).not.toContain("room-0@muc.example.com");
    expect(keys).not.toContain("room-4@muc.example.com");
    expect(keys).toContain("room-6@muc.example.com");
    expect(c.getRoomLastSeen("room-0@muc.example.com")).toBeUndefined();
  });
});
