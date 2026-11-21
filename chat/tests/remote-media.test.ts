import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { remoteMediaSource } from "../src/lib/remote-media";

const HOME = { accountDomain: "home.test", mucService: "muc.home.test", spacesService: "spaces.home.test" };

function message(partial: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "m1",
    author: "bob",
    body: "https://images.example/cat.png",
    createdAt: "2026-11-17T10:00:00Z",
    createdAtSource: "archive",
    isSelf: false,
    ...partial,
  } as TimelineMessage;
}

describe("media from other servers", () => {
  test("anything posted in a room on another server waits, whoever sent it", () => {
    expect(remoteMediaSource(message({ authorOccupantJid: "dev@chat.remote.test/bob" }), HOME)).toBe("chat.remote.test");
    expect(remoteMediaSource(message({
      authorOccupantJid: "dev@chat.remote.test/carol",
      authorRealJid: "carol@home.test",
    }), HOME)).toBe("chat.remote.test");
    expect(remoteMediaSource(message({ stanzaIdBy: "dev@chat.remote.test" }), HOME)).toBe("chat.remote.test");
  });

  test("a sender whose real address is on another domain waits, in a home room too", () => {
    expect(remoteMediaSource(message({
      authorOccupantJid: "general@muc.home.test/bob",
      authorRealJid: "bob@remote.test",
    }), HOME)).toBe("remote.test");
    // A direct message from an account elsewhere.
    expect(remoteMediaSource(message({ authorJid: "bob@remote.test", stanzaIdBy: "alice@home.test" }), HOME)).toBe("remote.test");
  });

  test("home senders, the user's own messages and undisclosed occupants of home rooms load as before", () => {
    expect(remoteMediaSource(message({
      authorOccupantJid: "general@muc.home.test/bob",
      authorRealJid: "bob@home.test",
    }), HOME)).toBeNull();
    expect(remoteMediaSource(message({ authorOccupantJid: "dev@chat.remote.test/alice", isSelf: true }), HOME)).toBeNull();
    expect(remoteMediaSource(message({
      authorOccupantJid: "general@muc.home.test/someone",
      authorJid: "general@muc.home.test/someone",
    }), HOME)).toBeNull();
  });

  test("before the home MUC service is known, media in a room that cannot be placed waits", () => {
    const undiscovered = { ...HOME, mucService: null };
    expect(remoteMediaSource(message({ authorOccupantJid: "dev@chat.remote.test/bob" }), undiscovered)).toBe("chat.remote.test");
    // The conventional home MUC name is no evidence either, until discovered.
    expect(remoteMediaSource(message({ authorOccupantJid: "general@muc.home.test/bob" }), undiscovered)).toBe("muc.home.test");
    expect(remoteMediaSource(message({ authorJid: "bob@remote.test" }), undiscovered)).toBe("remote.test");
    // Rooms on the account domain and the user's own messages still load.
    expect(remoteMediaSource(message({ authorOccupantJid: "general@home.test/bob" }), undiscovered)).toBeNull();
    expect(remoteMediaSource(message({ authorOccupantJid: "dev@chat.remote.test/bob", isSelf: true }), undiscovered)).toBeNull();
    // Once discovered, a home room's media loads again.
    expect(remoteMediaSource(message({ authorOccupantJid: "general@muc.home.test/bob" }), HOME)).toBeNull();
  });
});

describe("rooms on a component the home server lists", () => {
  test("load their media before the MUC service is identified", () => {
    const home = { accountDomain: "home.test", mucService: null, components: ["muc.home.test", "calls.home.test"] };
    expect(remoteMediaSource(message({ authorOccupantJid: "general@muc.home.test/bob" }), home)).toBeNull();
    // A room anywhere else still waits.
    expect(remoteMediaSource(message({ authorOccupantJid: "dev@chat.remote.test/bob" }), home)).toBe("chat.remote.test");
  });
});

describe("senders on a home component", () => {
  test("an extension bot posting from a component the home server lists loads its media", () => {
    const home = { ...HOME, components: ["muc.home.test", "extensions.home.test"] };
    expect(remoteMediaSource(message({ authorJid: "plugin@extensions.home.test" }), home)).toBeNull();
    expect(remoteMediaSource(message({
      authorOccupantJid: "general@muc.home.test/plugin",
      authorRealJid: "plugin@extensions.home.test",
    }), home)).toBeNull();
    // A sender on a discovered home service loads too.
    expect(remoteMediaSource(message({ authorJid: "bot@spaces.home.test" }), HOME)).toBeNull();
    // A component nobody listed is still another server's.
    expect(remoteMediaSource(message({ authorJid: "plugin@extensions.remote.test" }), home)).toBe("extensions.remote.test");
    expect(remoteMediaSource(message({ authorJid: "plugin@extensions.home.test" }), HOME)).toBe("extensions.home.test");
  });
});
