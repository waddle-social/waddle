import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import {
  $livePresence,
  resetContactPresence,
  setContactPresence,
  setOccupantPresence,
  setSelfPresenceJid,
} from "../src/presence/live-presence";
import { resetInCallOverlays, setInCallOverlay } from "../src/presence/in-call-overlay-store";
import { pickPresence, resetPresenceMode, setSelfBroadcastShow } from "../src/presence/presence-store";

afterEach(() => {
  resetContactPresence();
  setOccupantPresence({});
  setSelfPresenceJid(null);
  resetPresenceMode();
  resetInCallOverlays();
});

const AVATAR = "../src/components/ui/UserAvatar.vue";

describe("live presence by JID", () => {
  test("a contact's own presence wins over a room, which only fills in for people without it", () => {
    // Rooms only hear the Show a client joined with, so "online" there is not news.
    setContactPresence("Bob@example.com", "xa");
    setOccupantPresence({ "bob@example.com": ["online"], "carol@example.com": ["online", "dnd"] });
    setContactPresence("dave@example.com", "offline");

    expect($livePresence.get()).toEqual({
      "bob@example.com": "xa",
      "carol@example.com": "dnd",
      "dave@example.com": "offline",
    });
  });

  test("the signed-in account follows this device's chosen Show, then what it broadcasts", () => {
    setSelfPresenceJid("me@example.com/web");
    setContactPresence("me@example.com", "available");
    pickPresence("dnd");
    expect($livePresence.get()["me@example.com"]).toBe("dnd");

    // Auto-away is never a pick: only the broadcast carries it.
    pickPresence("reset");
    setSelfBroadcastShow("xa");
    expect($livePresence.get()["me@example.com"]).toBe("xa");
  });

  test("an avatar shows a known person's dot without its caller passing presence", async () => {
    setContactPresence("bob@example.com", "dnd");
    const known = await renderVueComponent(AVATAR, { name: "Bob", jid: "bob@example.com" }, import.meta.url);
    // A focused-room guess of "offline" never hides what the app knows.
    const overridden = await renderVueComponent(AVATAR, { name: "Bob", jid: "bob@example.com", presence: "offline" }, import.meta.url);
    const unknown = await renderVueComponent(AVATAR, { name: "Eve", jid: "eve@example.com" }, import.meta.url);

    expect(known).toContain(`data-presence="dnd"`);
    expect(overridden).toContain(`data-presence="dnd"`);
    expect(unknown).not.toContain("data-presence");
  });

  test("an avatar follows the person's in-call overlay unless its caller hides the badge", async () => {
    setInCallOverlay("bob@example.com", true);
    const shown = await renderVueComponent(AVATAR, { name: "Bob", jid: "bob@example.com" }, import.meta.url);
    const hidden = await renderVueComponent(AVATAR, { name: "Bob", jid: "bob@example.com", inCall: false }, import.meta.url);

    expect(shown).toContain(`aria-label="In a call"`);
    expect(hidden).not.toContain(`aria-label="In a call"`);
  });
});
