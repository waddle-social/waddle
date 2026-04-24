import { describe, expect, test } from "bun:test";
import { mentionMatchesUsername, resolveMentionUri } from "../src/lib/mentions";

describe("mention helpers", () => {
  test("matches mentions against usernames case-insensitively", () => {
    expect(mentionMatchesUsername("xmpp:Rawkode@waddle.social", "rawkode")).toBe(true);
    expect(mentionMatchesUsername("ICEPUMA", "icepuma")).toBe(true);
    expect(mentionMatchesUsername("randax@waddle.social", "rawkode")).toBe(false);
  });

  test("resolves room nick mentions through known member bare JIDs", () => {
    expect(resolveMentionUri("Bob", { bob: "bob@localhost" })).toBe("xmpp:bob@localhost");
    expect(resolveMentionUri("rawkode", { Rawkode: "Rawkode@waddle.social" })).toBe("xmpp:rawkode@waddle.social");
    expect(resolveMentionUri("icepuma")).toBe("xmpp:icepuma");
  });
});
