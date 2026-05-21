import { describe, expect, test } from "bun:test";
import {
  compareOctetStrings,
  incomingProposeWinsTieBreak,
  isSameCallBareJid,
} from "../src/lib/calls/tie-break";

describe("call tie-break helpers", () => {
  test("compares session ids with octet collation semantics", () => {
    expect(compareOctetStrings("a", "b")).toBeLessThan(0);
    expect(compareOctetStrings("b", "a")).toBeGreaterThan(0);
    expect(compareOctetStrings("a", "aa")).toBeLessThan(0);
    expect(compareOctetStrings("aa", "a")).toBeGreaterThan(0);
    expect(compareOctetStrings("c1", "c1")).toBe(0);
  });

  test("lower incoming sid wins the simultaneous propose tie-break", () => {
    expect(
      incomingProposeWinsTieBreak(
        "a-incoming",
        "z-outgoing",
        "bob@waddle.test/phone",
        "alice@waddle.test/web",
      ),
    ).toBe(true);
    expect(
      incomingProposeWinsTieBreak(
        "z-incoming",
        "a-outgoing",
        "bob@waddle.test/phone",
        "alice@waddle.test/web",
      ),
    ).toBe(false);
  });

  test("equal sid falls back to lower full JabberID", () => {
    expect(
      incomingProposeWinsTieBreak(
        "same",
        "same",
        "alice@waddle.test/phone",
        "bob@waddle.test/web",
      ),
    ).toBe(true);
    expect(
      incomingProposeWinsTieBreak(
        "same",
        "same",
        "carol@waddle.test/phone",
        "bob@waddle.test/web",
      ),
    ).toBe(false);
  });

  test("matches full resources by bare JID before applying tie-break", () => {
    expect(isSameCallBareJid("Bob@Waddle.Test/phone", "bob@waddle.test")).toBe(true);
    expect(isSameCallBareJid("carol@waddle.test/phone", "bob@waddle.test")).toBe(false);
  });
});
