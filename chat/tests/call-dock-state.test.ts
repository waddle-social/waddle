import { afterEach, describe, expect, test } from "bun:test";
import {
  $callDockOpen,
  closeCallDock,
  openCallDock,
  toggleCallDock,
} from "../src/lib/calls/call-dock-state";
import { $callState, clearCallState } from "../src/lib/calls/call-store";

afterEach(() => {
  clearCallState();
  closeCallDock();
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
