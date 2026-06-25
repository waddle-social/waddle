import { afterEach, describe, expect, test } from "bun:test";
import { effectScope } from "vue";

import {
  clearInCallOverlay,
  isInCall,
  resetInCallOverlays,
  setInCallOverlay,
  useInCallOverlay,
} from "../src/presence/in-call-overlay-store";

describe("in-call overlay store", () => {
  afterEach(() => {
    resetInCallOverlays();
  });

  test("a contact publishing the overlay reads as in a call", () => {
    setInCallOverlay("alice@waddle.test", true);
    expect(isInCall("alice@waddle.test")).toBe(true);
  });

  test("an unknown contact is not in a call", () => {
    expect(isInCall("ghost@waddle.test")).toBe(false);
  });

  test("retracting the overlay (set false) clears the badge", () => {
    setInCallOverlay("alice@waddle.test", true);
    setInCallOverlay("alice@waddle.test", false);
    expect(isInCall("alice@waddle.test")).toBe(false);
  });

  test("a full JID is collapsed to bare on write and read", () => {
    setInCallOverlay("alice@waddle.test/laptop", true);
    expect(isInCall("alice@waddle.test")).toBe(true);
    expect(isInCall("alice@waddle.test/phone")).toBe(true);
  });

  test("ghost cleanup: clearing on unavailable removes the overlay", () => {
    setInCallOverlay("alice@waddle.test", true);
    clearInCallOverlay("alice@waddle.test/laptop");
    expect(isInCall("alice@waddle.test")).toBe(false);
  });

  test("reset forgets every overlay (logout)", () => {
    setInCallOverlay("alice@waddle.test", true);
    setInCallOverlay("bob@waddle.test", true);
    resetInCallOverlays();
    expect(isInCall("alice@waddle.test")).toBe(false);
    expect(isInCall("bob@waddle.test")).toBe(false);
  });

  test("useInCallOverlay maps a contact to the current store state", () => {
    // bun:test has no `window`, so @nanostores/vue's useStore takes a one-time
    // snapshot rather than subscribing; a fresh read reflects each change.
    // In-browser reactivity is the library's window-guarded subscription.
    const scope = effectScope();
    scope.run(() => {
      expect(useInCallOverlay(() => "alice@waddle.test").value).toBe(false);
      setInCallOverlay("alice@waddle.test", true);
      expect(useInCallOverlay(() => "alice@waddle.test").value).toBe(true);
      clearInCallOverlay("alice@waddle.test");
      expect(useInCallOverlay(() => "alice@waddle.test").value).toBe(false);
    });
    scope.stop();
  });
});
