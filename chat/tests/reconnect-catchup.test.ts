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
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");
    c.recordRoomSeen("general@muc.example.com", "2024-01-02T00:00:00Z");

    // First session:started = initial login; skip catch-up.
    c.onSessionStarted();
    // Second session:started = resume after drop; catch up everywhere.
    const entries = c.onSessionStarted();

    expect(entries).toHaveLength(2);
    expect(entries).toContainEqual({ kind: "dm", key: "bob@example.com" });
    expect(entries).toContainEqual({
      kind: "room",
      key: "general@muc.example.com",
    });
  });

  test("getDmLastSeen returns the most recent timestamp recorded for a peer", () => {
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z");

    expect(c.getDmLastSeen("bob@example.com")).toBe("2024-01-02T00:00:00Z");
  });

  test("recordSeen ignores out-of-order (older) timestamps", () => {
    // Carbons / MAM results can arrive out of order. We must not rewind the
    // cursor, or the next catch-up will re-pull already-applied messages
    // and drown the UI in duplicates.
    const c = new ReconnectCatchup();
    c.recordDmSeen("bob@example.com", "2024-01-02T00:00:00Z");
    c.recordDmSeen("bob@example.com", "2024-01-01T00:00:00Z");

    expect(c.getDmLastSeen("bob@example.com")).toBe("2024-01-02T00:00:00Z");
  });

  test("DM and room namespaces are independent — same JID in both kinds does not collide", () => {
    // Defensive: a bare JID `foo@bar` could theoretically appear as both a
    // DM peer and a room. Their cursors must not interfere.
    const c = new ReconnectCatchup();
    c.recordDmSeen("foo@bar", "2024-01-01T00:00:00Z");
    c.recordRoomSeen("foo@bar", "2024-02-02T00:00:00Z");

    expect(c.getDmLastSeen("foo@bar")).toBe("2024-01-01T00:00:00Z");
    expect(c.getRoomLastSeen("foo@bar")).toBe("2024-02-02T00:00:00Z");
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
