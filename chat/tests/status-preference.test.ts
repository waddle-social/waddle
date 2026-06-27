import { describe, expect, test } from "bun:test";

import {
  parseStatusPreference,
  presenceModeFromWire,
  presenceModeToWire,
  statusPreferenceUpdate,
} from "../src/presence/status-preference";
import { withFakeDomParser } from "./helpers/disco-xml";

const NODE = "urn:waddle:status-preference:0";
const SELF = "me@waddle.test";

function opaque(xml: string) {
  return { payload: { kind: "opaque", xml } };
}
function event(from: string | undefined, items: Array<Record<string, unknown>>, node = NODE) {
  return { from, node, items } as Parameters<typeof statusPreferenceUpdate>[0];
}

describe("presenceModeFromWire / presenceModeToWire", () => {
  test("automatic round-trips", () => {
    expect(presenceModeFromWire({ mode: "automatic" })).toEqual({ kind: "automatic" });
    expect(presenceModeToWire({ kind: "automatic" })).toEqual({ mode: "automatic" });
  });

  test("each manual status round-trips", () => {
    // `chat` ("free for chat", ADR-010 Phase 5b) syncs across devices like the
    // other manual picks — both directions of the wire mapping must carry it.
    for (const status of ["available", "chat", "away", "dnd"] as const) {
      expect(presenceModeFromWire({ mode: "manual", status })).toEqual({ kind: "manual", status });
      expect(presenceModeToWire({ kind: "manual", status })).toEqual({ mode: "manual", status });
    }
  });

  test("invalid shapes map to null", () => {
    expect(presenceModeFromWire(null)).toBeNull();
    expect(presenceModeFromWire(undefined)).toBeNull();
    expect(presenceModeFromWire({ mode: "invisible" })).toBeNull();
    expect(presenceModeFromWire({ mode: "manual" })).toBeNull();
    expect(presenceModeFromWire({ mode: "manual", status: "xa" })).toBeNull();
  });
});

describe("statusPreferenceUpdate (incoming pubsub event → mode to adopt)", () => {
  test("our own manual pick from another resource is adopted", async () => {
    await withFakeDomParser(async () => {
      expect(
        statusPreferenceUpdate(
          event(`${SELF}/phone`, [
            opaque(`<status-preference xmlns="${NODE}" mode="manual" status="away"/>`),
          ]),
          SELF,
        ),
      ).toEqual({ kind: "manual", status: "away" });
    });
  });

  test("our own automatic (reset) from another resource is adopted", async () => {
    await withFakeDomParser(async () => {
      expect(
        statusPreferenceUpdate(
          event(`${SELF}/phone`, [opaque(`<status-preference xmlns="${NODE}" mode="automatic"/>`)]),
          SELF,
        ),
      ).toEqual({ kind: "automatic" });
    });
  });

  test("a retracted item reads as automatic", () => {
    expect(
      statusPreferenceUpdate(
        event(`${SELF}/phone`, [{ retracted: true, payload: { kind: "empty" } }]),
        SELF,
      ),
    ).toEqual({ kind: "automatic" });
  });

  test("a foreign node is ignored", () => {
    expect(
      statusPreferenceUpdate(
        event(`${SELF}/phone`, [{ payload: { kind: "empty" } }], "urn:xmpp:mood:0"),
        SELF,
      ),
    ).toBeNull();
  });

  test("an event with no sender is ignored", () => {
    expect(statusPreferenceUpdate(event(undefined, [{ payload: { kind: "empty" } }]), SELF)).toBeNull();
  });

  test("another user's preference is never adopted (owner-only)", async () => {
    await withFakeDomParser(async () => {
      expect(
        statusPreferenceUpdate(
          event("someone-else@waddle.test/x", [
            opaque(`<status-preference xmlns="${NODE}" mode="manual" status="dnd"/>`),
          ]),
          SELF,
        ),
      ).toBeNull();
    });
  });

  test("the last item wins for a multi-item event", async () => {
    await withFakeDomParser(async () => {
      const awayThenAuto = [
        opaque(`<status-preference xmlns="${NODE}" mode="manual" status="away"/>`),
        opaque(`<status-preference xmlns="${NODE}" mode="automatic"/>`),
      ];
      expect(statusPreferenceUpdate(event(`${SELF}/phone`, awayThenAuto), SELF)).toEqual({
        kind: "automatic",
      });
    });
  });
});

describe("parseStatusPreference", () => {
  test("parses manual + automatic XML", async () => {
    await withFakeDomParser(async () => {
      expect(parseStatusPreference(`<status-preference xmlns="${NODE}" mode="manual" status="dnd"/>`)).toEqual({
        kind: "manual",
        status: "dnd",
      });
      expect(parseStatusPreference(`<status-preference xmlns="${NODE}" mode="automatic"/>`)).toEqual({
        kind: "automatic",
      });
    });
  });

  test("returns null without a DOMParser", () => {
    expect(parseStatusPreference(`<status-preference xmlns="${NODE}" mode="automatic"/>`)).toBeNull();
  });
});
