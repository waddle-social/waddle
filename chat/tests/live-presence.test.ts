import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import {
  $livePresence,
  resetContactPresence,
  setContactPresence,
  setOccupantPresence,
  setSelfPresenceJid,
} from "../src/presence/live-presence";
import { pickPresence, resetPresenceMode } from "../src/presence/presence-store";

afterEach(() => {
  resetContactPresence();
  setOccupantPresence({});
  setSelfPresenceJid(null);
  resetPresenceMode();
});

describe("live presence by JID", () => {
  test("merges contact and room presence most-available-wins, DND first", () => {
    setContactPresence("Bob@example.com", "away");
    setOccupantPresence({ "bob@example.com": ["online"], "carol@example.com": ["away", "dnd"] });
    setContactPresence("dave@example.com", "offline");

    expect($livePresence.get()).toEqual({
      "bob@example.com": "online",
      "carol@example.com": "dnd",
      "dave@example.com": "offline",
    });
  });

  test("the signed-in account follows this device's chosen Show, not a room echo", () => {
    setSelfPresenceJid("me@example.com/web");
    setOccupantPresence({ "me@example.com": ["online"] });
    pickPresence("away");

    expect($livePresence.get()["me@example.com"]).toBe("away");
  });

  test("an avatar shows a known person's dot without its caller passing presence", async () => {
    setContactPresence("bob@example.com", "dnd");
    const known = await renderVueComponent("../src/components/ui/UserAvatar.vue", { name: "Bob", jid: "bob@example.com" }, import.meta.url);
    // A focused-room guess of "offline" never hides what the app knows.
    const overridden = await renderVueComponent(
      "../src/components/ui/UserAvatar.vue",
      { name: "Bob", jid: "bob@example.com", presence: "offline" },
      import.meta.url,
    );
    const unknown = await renderVueComponent("../src/components/ui/UserAvatar.vue", { name: "Eve", jid: "eve@example.com" }, import.meta.url);

    expect(known).toContain(`data-presence="dnd"`);
    expect(overridden).toContain(`data-presence="dnd"`);
    expect(unknown).not.toContain("data-presence");
  });
});
