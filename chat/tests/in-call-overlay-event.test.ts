import { describe, expect, test } from "bun:test";

import { activityOverlayUpdate } from "../src/presence/in-call-activity";
import { withFakeDomParser } from "./helpers/disco-xml";

const NODE = "http://jabber.org/protocol/activity";
const SELF = "me@waddle.test";
const NS = "http://jabber.org/protocol/activity";

function opaque(xml: string) {
  return { payload: { kind: "opaque", xml } };
}
function event(from: string | undefined, items: Array<Record<string, unknown>>, node = NODE) {
  return { from, node, items } as Parameters<typeof activityOverlayUpdate>[0];
}

describe("activityOverlayUpdate (incoming pubsub event → overlay update)", () => {
  test("a contact's talking/on_the_phone sets them in a call", async () => {
    await withFakeDomParser(async () => {
      expect(
        activityOverlayUpdate(event("alice@waddle.test", [opaque(`<activity xmlns="${NS}"><talking><on_the_phone/></talking></activity>`)]), SELF),
      ).toEqual({ jid: "alice@waddle.test", inCall: true });
    });
  });

  test("the empty activity retraction clears the contact", async () => {
    await withFakeDomParser(async () => {
      expect(
        activityOverlayUpdate(event("alice@waddle.test", [opaque(`<activity xmlns="${NS}"/>`)]), SELF),
      ).toEqual({ jid: "alice@waddle.test", inCall: false });
    });
  });

  test("a retracted item clears the contact", () => {
    expect(
      activityOverlayUpdate(event("alice@waddle.test", [{ retracted: true, payload: { kind: "empty" } }]), SELF),
    ).toEqual({ jid: "alice@waddle.test", inCall: false });
  });

  test("an event from a foreign node is ignored", () => {
    expect(
      activityOverlayUpdate(event("alice@waddle.test", [{ payload: { kind: "empty" } }], "urn:xmpp:mood:0"), SELF),
    ).toBeNull();
  });

  test("an event with no sender is ignored", () => {
    expect(activityOverlayUpdate(event(undefined, [{ payload: { kind: "empty" } }]), SELF)).toBeNull();
  });

  test("our own activity (self-echo) is never an overlay on ourselves", () => {
    expect(
      activityOverlayUpdate(event("me@waddle.test/laptop", [{ payload: { kind: "empty" } }]), SELF),
    ).toBeNull();
  });

  test("the last item wins for a multi-item event", async () => {
    await withFakeDomParser(async () => {
      const inThenOut = [
        opaque(`<activity xmlns="${NS}"><talking><on_the_phone/></talking></activity>`),
        opaque(`<activity xmlns="${NS}"/>`),
      ];
      expect(activityOverlayUpdate(event("alice@waddle.test", inThenOut), SELF)).toEqual({
        jid: "alice@waddle.test",
        inCall: false,
      });
    });
  });
});
