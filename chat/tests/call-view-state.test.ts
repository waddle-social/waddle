import { afterEach, describe, expect, test } from "bun:test";
import {
  $callPinnedTileKey,
  $callSelfViewHidden,
  $callViewMode,
  resetCallViewState,
  setCallViewMode,
  toggleCallPin,
  toggleCallSelfViewHidden,
} from "../src/lib/calls/call-view-state";
import { $callCamEnabled } from "../src/lib/calls/call-controls";

afterEach(() => {
  resetCallViewState();
  $callCamEnabled.set(true);
});

describe("call view state", () => {
  test("defaults to Gallery with nothing pinned", () => {
    expect($callViewMode.get()).toBe("gallery");
    expect($callPinnedTileKey.get()).toBeNull();
  });

  test("setCallViewMode switches the stage layout", () => {
    setCallViewMode("speaker");
    expect($callViewMode.get()).toBe("speaker");
  });

  test("toggleCallPin pins a tile and unpins the same tile", () => {
    toggleCallPin("remote:bob@example.com/web:camera");
    expect($callPinnedTileKey.get()).toBe("remote:bob@example.com/web:camera");

    toggleCallPin("remote:bob@example.com/web:camera");
    expect($callPinnedTileKey.get()).toBeNull();
  });

  test("toggleCallPin moves the pin to a different tile", () => {
    toggleCallPin("remote:bob@example.com/web:camera");
    toggleCallPin("remote:carol@example.com/web:camera");
    expect($callPinnedTileKey.get()).toBe("remote:carol@example.com/web:camera");
  });

  test("resetCallViewState returns to Gallery with nothing pinned", () => {
    setCallViewMode("speaker");
    toggleCallPin("remote:bob@example.com/web:camera");

    resetCallViewState();

    expect($callViewMode.get()).toBe("gallery");
    expect($callPinnedTileKey.get()).toBeNull();
  });

  test("self-view starts visible and toggleCallSelfViewHidden flips it both ways", () => {
    expect($callSelfViewHidden.get()).toBe(false);

    toggleCallSelfViewHidden();
    expect($callSelfViewHidden.get()).toBe(true);

    toggleCallSelfViewHidden();
    expect($callSelfViewHidden.get()).toBe(false);
  });

  test("resetCallViewState restores the self-view", () => {
    toggleCallSelfViewHidden();

    resetCallViewState();

    expect($callSelfViewHidden.get()).toBe(false);
  });

  test("hiding the self-view leaves the outgoing camera publishing untouched", () => {
    expect($callCamEnabled.get()).toBe(true);

    toggleCallSelfViewHidden();

    // Hide self-view is a local rendering choice only; it must never mute the
    // camera the way the cam toggle would.
    expect($callCamEnabled.get()).toBe(true);
  });
});
