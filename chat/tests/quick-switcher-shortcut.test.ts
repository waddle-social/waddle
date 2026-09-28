import { describe, expect, test } from "bun:test";
import { isQuickSwitcherShortcut } from "../src/shell/controllers/use-chat-keyboard";

function press(key: string, code: string, modifiers: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return { key, code, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...modifiers } as KeyboardEvent;
}

describe("isQuickSwitcherShortcut", () => {
  test("is Cmd+K on Apple platforms and Ctrl+K elsewhere", () => {
    expect(isQuickSwitcherShortcut(press("k", "KeyK", { metaKey: true }), true)).toBe(true);
    expect(isQuickSwitcherShortcut(press("k", "KeyK", { ctrlKey: true }), true)).toBe(false);
    expect(isQuickSwitcherShortcut(press("k", "KeyK", { ctrlKey: true }), false)).toBe(true);
    expect(isQuickSwitcherShortcut(press("k", "KeyK", { metaKey: true }), false)).toBe(false);
    expect(isQuickSwitcherShortcut(press("K", "KeyK", { ctrlKey: true }), false)).toBe(true);
  });

  test("rejects extra modifiers and plain K", () => {
    expect(isQuickSwitcherShortcut(press("k", "KeyK"), false)).toBe(false);
    expect(isQuickSwitcherShortcut(press("K", "KeyK", { ctrlKey: true, shiftKey: true }), false)).toBe(false);
    expect(isQuickSwitcherShortcut(press("k", "KeyK", { ctrlKey: true, altKey: true }), false)).toBe(false);
    expect(isQuickSwitcherShortcut(press("k", "KeyK", { ctrlKey: true, metaKey: true }), false)).toBe(false);
  });

  test("leaves composition shortcuts to the input method", () => {
    for (const isApplePlatform of [false, true]) {
      const modifier = isApplePlatform ? { metaKey: true } : { ctrlKey: true };
      expect(isQuickSwitcherShortcut(
        press("Process", "KeyK", { ...modifier, isComposing: true }), isApplePlatform,
      )).toBe(false);
      // Safari can report keyCode 229 without isComposing.
      expect(isQuickSwitcherShortcut(
        press("k", "KeyK", { ...modifier, keyCode: 229 }), isApplePlatform,
      )).toBe(false);
    }
  });

  test("follows the typed letter on Latin layouts and the physical key otherwise", () => {
    // Cyrillic layout: the K key types "л".
    expect(isQuickSwitcherShortcut(press("л", "KeyK", { ctrlKey: true }), false)).toBe(true);
    // Dvorak: "k" sits on the physical V key; the physical K key types "t".
    expect(isQuickSwitcherShortcut(press("k", "KeyV", { ctrlKey: true }), false)).toBe(true);
    expect(isQuickSwitcherShortcut(press("t", "KeyK", { ctrlKey: true }), false)).toBe(false);
  });
});
