import { afterEach, describe, expect, test } from "bun:test";

import { $presenceMode, pickPresence, resetPresenceMode } from "../src/presence/presence-store";

describe("presence-store", () => {
  afterEach(() => {
    $presenceMode.set({ kind: "automatic" });
  });

  test("pickPresence pins a manual status", () => {
    pickPresence("dnd");
    expect($presenceMode.get()).toEqual({ kind: "manual", status: "dnd" });
  });

  test("pickPresence reset returns to automatic", () => {
    pickPresence("away");
    pickPresence("reset");
    expect($presenceMode.get()).toEqual({ kind: "automatic" });
  });

  test("resetPresenceMode clears a manual status (logout)", () => {
    pickPresence("dnd");
    resetPresenceMode();
    expect($presenceMode.get()).toEqual({ kind: "automatic" });
  });
});
