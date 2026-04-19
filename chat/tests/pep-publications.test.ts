import { describe, expect, mock, test } from "bun:test";
import type { Agent } from "stanza";
import {
  publishActivity,
  publishMood,
  publishTune,
  retractActivity,
  retractMood,
  retractTune,
} from "../src/lib/xmpp/pep-publications";

function makeAgent() {
  return {
    publish: mock(() => Promise.resolve({})),
    publishMood: mock(() => Promise.resolve({})),
    publishActivity: mock(() => Promise.resolve({})),
    publishTune: mock(() => Promise.resolve({})),
  } as unknown as Agent & {
    publish: ReturnType<typeof mock>;
    publishMood: ReturnType<typeof mock>;
    publishActivity: ReturnType<typeof mock>;
    publishTune: ReturnType<typeof mock>;
  };
}

describe("PEP Mood (XEP-0107)", () => {
  test("publishMood forwards a typed kind + text", async () => {
    const xmpp = makeAgent();
    await publishMood(xmpp, { kind: "happy", text: "Yay" });
    expect(xmpp.publishMood).toHaveBeenCalledTimes(1);
    expect((xmpp.publishMood as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({
      value: "happy",
      text: "Yay",
    });
  });

  test("publishMood without text omits text", async () => {
    const xmpp = makeAgent();
    await publishMood(xmpp, { kind: "calm" });
    expect((xmpp.publishMood as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({
      value: "calm",
      text: undefined,
    });
  });

  test("retractMood publishes empty mood payload", async () => {
    const xmpp = makeAgent();
    await retractMood(xmpp);
    expect((xmpp.publishMood as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({});
  });
});

describe("PEP Activity (XEP-0108)", () => {
  test("publishActivity emits [general] when no specific", async () => {
    const xmpp = makeAgent();
    await publishActivity(xmpp, { general: "working" });
    expect((xmpp.publishActivity as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({
      activity: ["working"],
      text: undefined,
    });
  });

  test("publishActivity emits [general, specific] when specific provided", async () => {
    const xmpp = makeAgent();
    await publishActivity(xmpp, { general: "working", specific: "coding", text: "deep work" });
    expect((xmpp.publishActivity as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({
      activity: ["working", "coding"],
      text: "deep work",
    });
  });

  test("retractActivity publishes an empty activity item", async () => {
    const xmpp = makeAgent();
    await retractActivity(xmpp);
    expect(xmpp.publishActivity).toHaveBeenCalledTimes(0);
    expect((xmpp.publish as ReturnType<typeof mock>).mock.calls[0]).toEqual([
      "",
      "http://jabber.org/protocol/activity",
      { itemType: "http://jabber.org/protocol/activity" },
    ]);
  });
});

describe("PEP Tune (XEP-0118)", () => {
  test("publishTune forwards every optional field", async () => {
    const xmpp = makeAgent();
    await publishTune(xmpp, {
      artist: "The Beatles",
      title: "Come Together",
      source: "Abbey Road",
      length: 259,
      rating: 9,
      track: "1",
      uri: "https://example.com/t",
    });
    expect((xmpp.publishTune as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({
      artist: "The Beatles",
      title: "Come Together",
      source: "Abbey Road",
      length: 259,
      rating: 9,
      track: "1",
      uri: "https://example.com/t",
    });
  });

  test("publishTune rejects rating outside 1-10", async () => {
    const xmpp = makeAgent();
    await expect(publishTune(xmpp, { rating: 11 })).rejects.toThrow("Tune rating");
    expect(xmpp.publishTune).toHaveBeenCalledTimes(0);
  });

  test("retractTune publishes empty tune payload", async () => {
    const xmpp = makeAgent();
    await retractTune(xmpp);
    expect((xmpp.publishTune as ReturnType<typeof mock>).mock.calls[0][0]).toEqual({});
  });
});
