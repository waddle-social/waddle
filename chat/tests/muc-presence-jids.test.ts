import { describe, expect, test } from "bun:test";
import { mucPresenceRealBareJid } from "../src/lib/xmpp/client";

class TestJid {
  constructor(private readonly value: string) {}

  toString() {
    return this.value;
  }
}

describe("mucPresenceRealBareJid", () => {
  test("normalizes top-level stanza.js muc.jid values", () => {
    expect(mucPresenceRealBareJid({ muc: { jid: "bob@example.com/web" } })).toBe("bob@example.com");
  });

  test("normalizes muc#user item jid values", () => {
    expect(mucPresenceRealBareJid({ muc: { item: { jid: "bob@example.com/web" } } })).toBe("bob@example.com");
  });

  test("normalizes muc#user items jid values", () => {
    expect(mucPresenceRealBareJid({ muc: { items: [{ jid: "bob@example.com/web" }] } })).toBe("bob@example.com");
  });

  test("accepts stanza JID objects", () => {
    expect(mucPresenceRealBareJid({
      muc: { item: { jid: new TestJid("bob@example.com/web") } },
    })).toBe("bob@example.com");
  });

  test("returns null when presence has no real JID", () => {
    expect(mucPresenceRealBareJid({ muc: { affiliation: "member", role: "participant" } })).toBeNull();
    expect(mucPresenceRealBareJid({})).toBeNull();
  });

  test("returns null for malformed real JID values", () => {
    expect(mucPresenceRealBareJid({ muc: { item: { jid: "not-a-jid" } } })).toBeNull();
    expect(mucPresenceRealBareJid({ muc: { item: { jid: {} } } })).toBeNull();
  });
});
