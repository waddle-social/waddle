import { afterEach, describe, expect, test } from "bun:test";
import {
  $callDockOpen,
  $callDockTab,
  closeCallDock,
  openCallDock,
  setCallDockTab,
  toggleCallChat,
  toggleCallDock,
  toggleCallParticipants,
} from "../src/lib/calls/call-dock-state";
import { $callState, clearCallState } from "../src/lib/calls/call-store";

afterEach(() => {
  clearCallState();
  closeCallDock();
  setCallDockTab("participants");
});

describe("call dock state", () => {
  test("defaults to closed; open/close/toggle mutate it", () => {
    expect($callDockOpen.get()).toBe(false);

    openCallDock();
    expect($callDockOpen.get()).toBe(true);

    toggleCallDock();
    expect($callDockOpen.get()).toBe(false);

    toggleCallDock();
    expect($callDockOpen.get()).toBe(true);

    closeCallDock();
    expect($callDockOpen.get()).toBe(false);
  });

  test("closes automatically when the call ends so the next call defaults closed", () => {
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid: "c1",
      media: { audio: true, video: false },
      join: { url: "wss://livekit.test", room: "room@muc.waddle.test", identity: "me", token: "jwt" },
      kind: "muc",
      selfNick: "me",
      selfFullJid: "me@waddle.test/browser",
    });
    openCallDock();
    expect($callDockOpen.get()).toBe(true);

    clearCallState();
    expect($callDockOpen.get()).toBe(false);
  });
});

describe("call dock tab", () => {
  test("defaults to the participants tab", () => {
    expect($callDockTab.get()).toBe("participants");
  });

  test("toggleCallChat opens the dock on Chat, switches to it, then closes on repeat", () => {
    // Closed → opens straight to the Chat tab.
    toggleCallChat();
    expect($callDockOpen.get()).toBe(true);
    expect($callDockTab.get()).toBe("chat");

    // Already open on Chat → closes the dock.
    toggleCallChat();
    expect($callDockOpen.get()).toBe(false);

    // Open on Participants → switches to Chat without closing.
    toggleCallParticipants();
    expect($callDockTab.get()).toBe("participants");
    toggleCallChat();
    expect($callDockOpen.get()).toBe(true);
    expect($callDockTab.get()).toBe("chat");
  });

  test("toggleCallParticipants mirrors the behaviour for the Participants tab", () => {
    toggleCallParticipants();
    expect($callDockOpen.get()).toBe(true);
    expect($callDockTab.get()).toBe("participants");

    toggleCallParticipants();
    expect($callDockOpen.get()).toBe(false);
  });

  test("resets to the participants tab when the call ends", () => {
    setCallDockTab("chat");
    expect($callDockTab.get()).toBe("chat");

    clearCallState();
    expect($callDockTab.get()).toBe("participants");
  });
});
