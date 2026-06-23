import { describe, expect, test } from "bun:test";
import {
  describeCallShortcutTarget,
  resolveCallShortcut,
  type CallShortcutInput,
} from "../src/lib/calls/call-shortcuts";

/**
 * Stand-in for a focused DOM element: `closest(selectors)` matches when any
 * comma-separated selector is in `matches`, mirroring `Element.closest`.
 */
function fakeTarget(matches: string[]): { closest(selectors: string): Element | null } {
  return {
    closest(selectors: string): Element | null {
      const wanted = selectors.split(",").map((part) => part.trim());
      return wanted.some((selector) => matches.includes(selector))
        ? ({} as Element)
        : null;
    },
  };
}

/**
 * A focused, non-editable keydown on a live call surface — the baseline
 * descriptor each behaviour test overrides the one field it cares about.
 */
function keydown(overrides: Partial<CallShortcutInput> = {}): CallShortcutInput {
  return {
    key: "",
    type: "keydown",
    repeat: false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    editableTarget: false,
    activationTarget: false,
    surfaceFocused: true,
    ...overrides,
  };
}

describe("resolveCallShortcut", () => {
  test("M toggles the mic", () => {
    expect(resolveCallShortcut(keydown({ key: "m" }))).toBe("toggle-mic");
  });

  test("holding Space starts push-to-talk", () => {
    expect(resolveCallShortcut(keydown({ key: " " }))).toBe("push-to-talk-start");
  });

  test("releasing Space ends push-to-talk", () => {
    expect(resolveCallShortcut(keydown({ key: " ", type: "keyup" }))).toBe(
      "push-to-talk-end",
    );
  });

  test("V toggles the camera", () => {
    expect(resolveCallShortcut(keydown({ key: "v" }))).toBe("toggle-camera");
  });

  test("S toggles screen share", () => {
    expect(resolveCallShortcut(keydown({ key: "s" }))).toBe("toggle-share");
  });

  test("R toggles raise hand", () => {
    expect(resolveCallShortcut(keydown({ key: "r" }))).toBe("toggle-raise-hand");
  });

  test("F enters immersive/fullscreen", () => {
    expect(resolveCallShortcut(keydown({ key: "f" }))).toBe("enter-immersive");
  });

  test("Escape exits immersive", () => {
    expect(resolveCallShortcut(keydown({ key: "Escape" }))).toBe("exit-immersive");
  });

  test("auto-repeat keydown does not re-fire a toggle", () => {
    expect(resolveCallShortcut(keydown({ key: "m", repeat: true }))).toBeNull();
  });

  test("auto-repeat does not re-start push-to-talk", () => {
    expect(resolveCallShortcut(keydown({ key: " ", repeat: true }))).toBeNull();
  });

  test("ignores modifier combinations so OS/browser shortcuts pass through", () => {
    expect(resolveCallShortcut(keydown({ key: "m", metaKey: true }))).toBeNull();
    expect(resolveCallShortcut(keydown({ key: "m", ctrlKey: true }))).toBeNull();
    expect(resolveCallShortcut(keydown({ key: "s", altKey: true }))).toBeNull();
  });

  test("stays inert while typing in chat", () => {
    expect(resolveCallShortcut(keydown({ key: "m", editableTarget: true }))).toBeNull();
    // Even Space must not push-to-talk while composing a message.
    expect(resolveCallShortcut(keydown({ key: " ", editableTarget: true }))).toBeNull();
  });

  test("only fires when a call surface is focused", () => {
    expect(resolveCallShortcut(keydown({ key: "m", surfaceFocused: false }))).toBeNull();
    expect(resolveCallShortcut(keydown({ key: " ", surfaceFocused: false }))).toBeNull();
  });

  test("Space on a focused control passes through to native activation", () => {
    // A focused Mute/Hang-up button must still activate on Space — don't
    // hijack it for push-to-talk (keydown AND keyup, since buttons fire on up).
    expect(resolveCallShortcut(keydown({ key: " ", activationTarget: true }))).toBeNull();
    expect(
      resolveCallShortcut(keydown({ key: " ", type: "keyup", activationTarget: true })),
    ).toBeNull();
  });

  test("letter shortcuts still fire while a control is focused", () => {
    // Letters don't collide with native button activation, so they still work.
    expect(resolveCallShortcut(keydown({ key: "m", activationTarget: true }))).toBe(
      "toggle-mic",
    );
  });
});

describe("describeCallShortcutTarget", () => {
  test("a null target is neither focused nor editable nor an activation target", () => {
    expect(describeCallShortcutTarget(null)).toEqual({
      surfaceFocused: false,
      editableTarget: false,
      activationTarget: false,
    });
  });

  test("focus inside the split surface counts as a focused call surface", () => {
    expect(describeCallShortcutTarget(fakeTarget([".call-split"]))).toEqual({
      surfaceFocused: true,
      editableTarget: false,
      activationTarget: false,
    });
  });

  test("a focused button is flagged as an activation target", () => {
    expect(describeCallShortcutTarget(fakeTarget(["button"])).activationTarget).toBe(true);
    expect(
      describeCallShortcutTarget(fakeTarget(['[role="button"]'])).activationTarget,
    ).toBe(true);
  });

  test("focus inside the expanded/immersive surface counts too", () => {
    expect(describeCallShortcutTarget(fakeTarget([".call-expanded"])).surfaceFocused).toBe(
      true,
    );
  });

  test("the document picture-in-picture window is not treated as a focused surface", () => {
    // It is a separate Window; its key events never reach this listener.
    expect(
      describeCallShortcutTarget(fakeTarget([".call-pip-document"])).surfaceFocused,
    ).toBe(false);
  });

  test("an editable element is flagged so typing stays inert", () => {
    expect(describeCallShortcutTarget(fakeTarget(["textarea"])).editableTarget).toBe(true);
    expect(
      describeCallShortcutTarget(fakeTarget(["[contenteditable='true']"])).editableTarget,
    ).toBe(true);
  });

  test("a chat composer outside any call surface is editable but not focused", () => {
    expect(describeCallShortcutTarget(fakeTarget(["[contenteditable='true']"]))).toEqual({
      surfaceFocused: false,
      editableTarget: true,
      activationTarget: false,
    });
  });
});
