import { afterEach, describe, expect, test } from "bun:test";
import {
  $callPinnedTileKey,
  $callViewMode,
  resetCallViewState,
  setCallViewMode,
  toggleCallPin,
} from "../src/lib/calls/call-view-state";

afterEach(() => {
  resetCallViewState();
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
});
