import { describe, expect, test } from "bun:test";
import {
  decodePersistedSmResumeState,
  type XmppResumeEntry,
  type XmppResumeXmlAttribute,
  type XmppResumeXmlToken,
} from "../src/lib/xmpp/sm-resume-types";

const SENT_AT = Date.parse("2026-07-17T12:00:00.000Z");

function start(
  localName: string,
  namespace = "jabber:client",
  attributes: XmppResumeXmlAttribute[] = [],
): XmppResumeXmlToken {
  return {
    kind: "start",
    name: { namespace, localName },
    attributes,
  };
}

function stateWithTokens(tokens: XmppResumeXmlToken[]): unknown {
  return {
    previd: "stream-1",
    inboundH: 0,
    outboundH: 0xFFFF_FFFF,
    maxResumeSeconds: 300,
    unhandledOutboundEntries: [{
      stanza: {
        stanzaKind: "message",
        tokens,
      },
      sentAtEpochMs: SENT_AT,
    }],
  };
}

function entryWithTokens(tokens: XmppResumeXmlToken[]): XmppResumeEntry {
  return {
    stanza: { stanzaKind: "message", tokens },
    sentAtEpochMs: SENT_AT,
  };
}

function stateWithEntries(unhandledOutboundEntries: XmppResumeEntry[]): unknown {
  return {
    previd: "stream-1",
    inboundH: 0,
    outboundH: 0,
    unhandledOutboundEntries,
  };
}

function expectDataError(value: unknown): void {
  let error: unknown;
  try {
    decodePersistedSmResumeState(value);
  } catch (cause) {
    error = cause;
  }
  expect(error).toBeInstanceOf(DOMException);
  expect((error as DOMException).name).toBe("DataError");
}

describe("durable XEP-0198 semantic decoder", () => {
  test("rebuilds bounded typed state and accepts the full u32 range", () => {
    const decoded = decodePersistedSmResumeState(stateWithTokens([
      start("message"),
      start("a\u203Fb", "urn:test:names"),
      { kind: "text", value: "hello" },
      { kind: "end" },
      { kind: "end" },
    ]));

    expect(decoded.outboundH).toBe(0xFFFF_FFFF);
    expect(decoded.unhandledOutboundEntries[0]?.stanza.tokens[1]).toEqual(
      start("a\u203Fb", "urn:test:names"),
    );
  });

  test("rejects counters outside u32 before a durable graph is returned", () => {
    expectDataError({
      previd: "stream-1",
      inboundH: -1,
      outboundH: 0,
      unhandledOutboundEntries: [],
    });
    expectDataError({
      previd: "stream-1",
      inboundH: 0,
      outboundH: 0x1_0000_0000,
      unhandledOutboundEntries: [],
    });
    expectDataError({
      previd: "stream-1",
      inboundH: 0,
      outboundH: 0,
      maxResumeSeconds: 0,
      unhandledOutboundEntries: [],
    });
  });

  test("requires the ordered outbound-entry array even when it is empty", () => {
    expectDataError({ previd: "stream-1", inboundH: 0, outboundH: 0 });
  });

  test("rejects root mismatch, unbalanced depth, and text outside the root", () => {
    expectDataError(stateWithTokens([
      start("presence"),
      { kind: "end" },
    ]));
    expectDataError(stateWithTokens([
      start("message"),
      start("body"),
      { kind: "end" },
    ]));
    expectDataError(stateWithTokens([
      start("message"),
      { kind: "end" },
      { kind: "text", value: "outside" },
    ]));
  });

  test("enforces aggregate XML token, depth, attribute, and UTF-8 budgets", () => {
    const nestedStarts = Array.from(
      { length: 64 },
      (_, index) => start(`n${index}`, "urn:test:depth"),
    );
    expectDataError(stateWithTokens([
      start("message"),
      ...nestedStarts,
      ...nestedStarts.map(() => ({ kind: "end" }) as const),
      { kind: "end" },
    ]));

    expectDataError(stateWithTokens([
      start("message"),
      ...Array.from(
        { length: 16_384 },
        () => ({ kind: "text", value: "" }) as const,
      ),
      { kind: "end" },
    ]));

    const attribute = {
      name: { namespace: "", localName: "a" },
      value: "",
    };
    expectDataError(stateWithTokens([
      start("message", "jabber:client", Array.from(
        { length: 16_385 },
        () => attribute,
      )),
      { kind: "end" },
    ]));

    expectDataError(stateWithTokens([
      start("message"),
      { kind: "text", value: "x".repeat(1024 * 1024 + 1) },
      { kind: "end" },
    ]));
  });

  test("accepts exactly 4,096 entries and rejects entry 4,097", () => {
    const exactEntries = Array.from({ length: 4_096 }, () => entryWithTokens([
      start("message"),
      { kind: "text", value: "" },
      { kind: "text", value: "" },
      { kind: "end" },
    ]));
    expect(decodePersistedSmResumeState(stateWithEntries(exactEntries))
      .unhandledOutboundEntries).toHaveLength(4_096);

    expectDataError(stateWithEntries([
      ...exactEntries,
      entryWithTokens([start("message"), { kind: "end" }]),
    ]));
  });

  test("shares exact token, attribute, and UTF-8 budgets across entries", () => {
    const exactTokens = Array.from({ length: 4_096 }, () => entryWithTokens([
      start("message"),
      { kind: "text", value: "" },
      { kind: "text", value: "" },
      { kind: "end" },
    ]));
    decodePersistedSmResumeState(stateWithEntries(exactTokens));
    exactTokens[0]!.stanza.tokens.splice(1, 0, { kind: "text", value: "" });
    expectDataError(stateWithEntries(exactTokens));

    const attributes = Array.from({ length: 4 }, (_, index) => ({
      name: { namespace: "", localName: `a${index}` },
      value: "",
    }));
    const exactAttributes = Array.from({ length: 4_096 }, () => entryWithTokens([
      start("message", "jabber:client", attributes),
      { kind: "end" },
    ]));
    decodePersistedSmResumeState(stateWithEntries(exactAttributes));
    exactAttributes[0]!.stanza.tokens = [
      start("message", "jabber:client", [
        ...attributes,
        { name: { namespace: "", localName: "a4" }, value: "" },
      ]),
      { kind: "end" },
    ];
    expectDataError(stateWithEntries(exactAttributes));

    const rootBytes = 2 * ("jabber:client".length + "message".length);
    const exactText = "x".repeat(1024 * 1024 - rootBytes);
    const exactBytes = [
      entryWithTokens([start("message"), { kind: "text", value: exactText }, { kind: "end" }]),
      entryWithTokens([start("message"), { kind: "end" }]),
    ];
    decodePersistedSmResumeState(stateWithEntries(exactBytes));
    exactBytes[1]!.stanza.tokens.splice(1, 0, { kind: "text", value: "x" });
    expectDataError(stateWithEntries(exactBytes));
  });

  test("accepts timestamp epoch/current/JS-Date-limit and rejects invalid numbers", () => {
    const timestampState = stateWithTokens([
      start("message"),
      { kind: "end" },
    ]) as { unhandledOutboundEntries: Array<{ sentAtEpochMs: number }> };
    for (const timestamp of [0, Date.now(), 8_640_000_000_000_000]) {
      timestampState.unhandledOutboundEntries[0]!.sentAtEpochMs = timestamp;
      decodePersistedSmResumeState(timestampState);
    }
    for (const timestamp of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 53]) {
      timestampState.unhandledOutboundEntries[0]!.sentAtEpochMs = timestamp;
      expectDataError(timestampState);
    }
  });

  test("rejects duplicate expanded attributes and invalid XML NCNames", () => {
    const duplicate = {
      name: { namespace: "urn:test:attribute", localName: "same" },
      value: "value",
    };
    expectDataError(stateWithTokens([
      start("message", "jabber:client", [duplicate, duplicate]),
      { kind: "end" },
    ]));
    expectDataError(stateWithTokens([
      start("message"),
      start("has:colon", "urn:test:names"),
      { kind: "end" },
      { kind: "end" },
    ]));
    expectDataError(stateWithTokens([
      start("message"),
      start("1starts-with-digit", "urn:test:names"),
      { kind: "end" },
      { kind: "end" },
    ]));
  });

  test("rejects invalid timestamps, XML controls, lone surrogates, and custom prototypes", () => {
    const invalidTimestamp = stateWithTokens([
      start("message"),
      { kind: "end" },
    ]) as {
      unhandledOutboundEntries: Array<{ sentAtEpochMs: number }>;
    };
    invalidTimestamp.unhandledOutboundEntries[0]!.sentAtEpochMs =
      8_640_000_000_000_001;
    expectDataError(invalidTimestamp);

    expectDataError({
      previd: "\uD800",
      inboundH: 0,
      outboundH: 0,
      unhandledOutboundEntries: [],
    });
    expectDataError({
      previd: "stream\u0000one",
      inboundH: 0,
      outboundH: 0,
      unhandledOutboundEntries: [],
    });

    const customPrototype = Object.create({ inherited: true }) as Record<string, unknown>;
    Object.assign(customPrototype, {
      previd: "stream-1",
      inboundH: 0,
      outboundH: 0,
      unhandledOutboundEntries: [],
    });
    expectDataError(customPrototype);
  });
});
