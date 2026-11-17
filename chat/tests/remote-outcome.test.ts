import { describe, expect, test } from "bun:test";
import {
  classifyRemoteError,
  classifyRemoteFailure,
  describeRemoteFailure,
} from "../src/lib/xmpp/remote-outcome";
import { RoomJoinError } from "../src/lib/xmpp/room-join-error";
import { DiscoTimeoutError } from "../src/lib/xmpp/discovery";

describe("remote failure classification", () => {
  test("an unreachable server is an outage, retried automatically when it says to wait", () => {
    expect(classifyRemoteFailure({ condition: "remote-server-timeout", errorType: "wait" })).toEqual({
      kind: "outage",
      retry: "automatic",
      condition: "remote-server-timeout",
      errorType: "wait",
    });
  });

  test("a server that cannot be found is an outage the user retries", () => {
    // Classified by condition: it arrives with type `cancel`.
    const failure = classifyRemoteFailure({ condition: "remote-server-not-found", errorType: "cancel" });
    expect(failure.kind).toBe("outage");
    expect(failure.retry).toBe("manual");
  });

  test("no answer at all is an outage", () => {
    expect(classifyRemoteFailure({ timedOut: true })).toEqual({ kind: "outage", retry: "automatic" });
  });

  test("a refusal is a denial that never retries on its own", () => {
    for (const condition of ["forbidden", "registration-required", "not-authorized", "subscription-required", "not-allowed"]) {
      const failure = classifyRemoteFailure({ condition, errorType: "auth" });
      expect(failure.kind).toBe("denial");
      expect(failure.retry).toBe("manual");
    }
  });

  test("an unknown address is a deletion that is not retried", () => {
    for (const condition of ["item-not-found", "gone"]) {
      expect(classifyRemoteFailure({ condition, errorType: "cancel" })).toMatchObject({ kind: "deletion", retry: "none" });
    }
  });

  test("outage, denial and deletion never share a kind", () => {
    const kinds = new Set([
      classifyRemoteFailure({ condition: "remote-server-timeout" }).kind,
      classifyRemoteFailure({ condition: "forbidden" }).kind,
      classifyRemoteFailure({ condition: "item-not-found" }).kind,
    ]);
    expect(kinds.size).toBe(3);
  });

  test("conditions outside the lists fall back on the error type", () => {
    expect(classifyRemoteFailure({ condition: "undefined-condition", errorType: "wait" }).kind).toBe("outage");
    expect(classifyRemoteFailure({ condition: "undefined-condition", errorType: "auth" }).kind).toBe("denial");
    expect(classifyRemoteFailure({ condition: "undefined-condition", errorType: "cancel" }).kind).toBe("unknown");
  });

  test("rejected requests are read through their stanza error", () => {
    const iqError = Object.assign(new Error("refused"), { condition: "forbidden", errorType: "auth" });
    expect(classifyRemoteError(iqError).kind).toBe("denial");
    expect(classifyRemoteError(new DiscoTimeoutError("chat.example.org", undefined, 35_000)).kind).toBe("outage");
    expect(classifyRemoteError(new RoomJoinError("timed out", { roomJid: "r@chat.example.org", timedOut: true })).kind).toBe("outage");
    expect(
      classifyRemoteError(new RoomJoinError("no", { roomJid: "r@chat.example.org", condition: "item-not-found", errorType: "cancel" })).kind,
    ).toBe("deletion");
  });
});

describe("remote failure copy", () => {
  test("a deletion does not claim the community was deleted", () => {
    const copy = describeRemoteFailure({ kind: "deletion", retry: "none" }, "room");
    expect(copy.detail).toContain("not open to accounts on other servers");
  });

  test("an outage promises a retry only when one is scheduled", () => {
    expect(describeRemoteFailure({ kind: "outage", retry: "automatic" }, "space").detail).toContain("try again");
    expect(describeRemoteFailure({ kind: "outage", retry: "manual" }, "space").detail).toContain("could not be found");
  });

  test("a denial says retrying will not help on its own", () => {
    expect(describeRemoteFailure({ kind: "denial", retry: "manual", condition: "forbidden" }, "room").detail)
      .toContain("Retrying will not help");
  });
});
