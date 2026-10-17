import { readFileSync } from "node:fs";
import { describe, expect, test } from "bun:test";
import { effectScope } from "vue";
import { shouldOpenThreadOnRowTap, tapTargetIsInteractive } from "../src/components/chat/message-row-tap";
import { useMessageGestures } from "../src/components/chat/composables/use-message-gestures";

// Minimal element model: `closest` walks parents matching a tag name or
// attribute presence, which is all the tap filter needs.
interface FakeElement {
  tag: string;
  attrs: Record<string, string>;
  parentElement: FakeElement | null;
  closest(selector: string): FakeElement | null;
  contains(other: unknown): boolean;
}

function matches(el: FakeElement, selector: string): boolean {
  return selector.split(",").map((part) => part.trim()).some((part) => {
    const attr = /^\[([a-z-]+)(?:='([^']*)')?\]$/.exec(part);
    if (attr) {
      const [, name, value] = attr;
      return name! in el.attrs && (value === undefined || el.attrs[name!] === value);
    }
    const tagWithAttr = /^([a-z]+)\[([a-z-]+)\]$/.exec(part);
    if (tagWithAttr) return el.tag === tagWithAttr[1] && tagWithAttr[2]! in el.attrs;
    return el.tag === part;
  });
}

function el(tag: string, parent: FakeElement | null = null, attrs: Record<string, string> = {}): FakeElement {
  const node: FakeElement = {
    tag,
    attrs,
    parentElement: parent,
    closest(selector) {
      for (let cur: FakeElement | null = node; cur; cur = cur.parentElement) {
        if (matches(cur, selector)) return cur;
      }
      return null;
    },
    contains(other) {
      for (let cur = other as FakeElement | null; cur; cur = cur.parentElement) {
        if (cur === node) return true;
      }
      return false;
    },
  };
  return node;
}

describe("tapTargetIsInteractive", () => {
  const timeline = el("div", null, { role: "button", tabindex: "0" });
  const row = el("div", timeline);
  const body = el("div", row);
  const paragraph = el("p", body);

  test("plain body text is not interactive, even under an interactive ancestor of the row", () => {
    expect(tapTargetIsInteractive(paragraph, row)).toBe(false);
    expect(tapTargetIsInteractive(body, row)).toBe(false);
    expect(tapTargetIsInteractive(row, row)).toBe(false);
  });

  test("links, buttons, media, role=button and opted-out nodes own their taps", () => {
    const link = el("a", paragraph, { href: "https://example.com" });
    const linkText = el("span", link);
    const replyQuote = el("button", row);
    const reaction = el("span", el("button", row));
    const image = el("img", body);
    const video = el("video", body);
    const chip = el("div", body, { role: "button" });
    const optedOut = el("span", el("div", body, { "data-row-tap-ignore": "" }));
    for (const target of [link, linkText, replyQuote, reaction, image, video, chip, optedOut]) {
      expect(tapTargetIsInteractive(target, row)).toBe(true);
    }
  });

  test("anchors without href and text nodes resolve sensibly", () => {
    expect(tapTargetIsInteractive(el("a", paragraph), row)).toBe(false);
    const textNode = { parentElement: el("button", row) };
    expect(tapTargetIsInteractive(textNode, row)).toBe(true);
    expect(tapTargetIsInteractive(null, row)).toBe(false);
  });
});

describe("shouldOpenThreadOnRowTap", () => {
  const tap = {
    opensThread: true,
    coarsePointer: true,
    stationaryTouch: true,
    hasTextSelection: false,
    targetIsInteractive: false,
  };

  test("a plain touch tap on a post with replies opens the thread", () => {
    expect(shouldOpenThreadOnRowTap(tap)).toBe(true);
  });

  test("everything else leaves the tap alone", () => {
    expect(shouldOpenThreadOnRowTap({ ...tap, opensThread: false })).toBe(false);
    expect(shouldOpenThreadOnRowTap({ ...tap, coarsePointer: false })).toBe(false);
    expect(shouldOpenThreadOnRowTap({ ...tap, stationaryTouch: false })).toBe(false);
    expect(shouldOpenThreadOnRowTap({ ...tap, hasTextSelection: true })).toBe(false);
    expect(shouldOpenThreadOnRowTap({ ...tap, targetIsInteractive: true })).toBe(false);
  });
});

describe("useMessageGestures stationary touch", () => {
  function pointer(type: "touch" | "mouse", x = 0, y = 0, pointerId = 1): PointerEvent {
    return { pointerId, pointerType: type, clientX: x, clientY: y, target: null, preventDefault() {} } as unknown as PointerEvent;
  }

  function setup() {
    const scope = effectScope();
    const gestures = scope.run(() =>
      useMessageGestures({ onLongPress() {}, onSwipeLeft() {}, onSwipeRight() {} }),
    )!;
    return { gestures, stop: () => scope.stop() };
  }

  test("a touch that stays put is a tap; a mouse click is not", () => {
    const { gestures, stop } = setup();
    expect(gestures.lastPressWasStationaryTouch()).toBe(false);
    gestures.handlers.onPointerdown(pointer("touch", 10, 10));
    gestures.handlers.onPointermove(pointer("touch", 14, 12));
    gestures.handlers.onPointerup(pointer("touch", 14, 12));
    expect(gestures.lastPressWasStationaryTouch()).toBe(true);
    gestures.handlers.onPointerdown(pointer("mouse", 10, 10));
    gestures.handlers.onPointerup(pointer("mouse", 10, 10));
    expect(gestures.lastPressWasStationaryTouch()).toBe(false);
    stop();
  });

  test("a swipe or a browser-taken scroll is not a tap", () => {
    const { gestures, stop } = setup();
    gestures.handlers.onPointerdown(pointer("touch", 100, 10));
    gestures.handlers.onPointermove(pointer("touch", 20, 12));
    gestures.handlers.onPointerup(pointer("touch", 20, 12));
    expect(gestures.lastPressWasStationaryTouch()).toBe(false);

    gestures.handlers.onPointerdown(pointer("touch", 10, 10));
    gestures.handlers.onPointercancel(pointer("touch", 10, 10));
    expect(gestures.lastPressWasStationaryTouch()).toBe(false);
    stop();
  });
});

describe("thread pane profiles", () => {
  const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

  test("thread rows forward avatar and name taps to the timeline's profile drawer", () => {
    const threadPanel = read("../src/components/chat/ThreadPanel.vue");
    const shell = read("../src/components/chat/ChatReadyShell.vue");
    const contentArea = read("../src/components/chat/ContentArea.vue");
    // Root row and reply rows both forward the event.
    expect(threadPanel.match(/@avatar-click="\(author, jid\) => emit\('avatarClick', author, jid\)"/g) ?? []).toHaveLength(2);
    // Parent (read-only) and active thread panes both route it.
    expect(shell.match(/@avatar-click="openProfileFromThread"/g) ?? []).toHaveLength(2);
    expect(shell).toContain("contentAreaRef.value?.openProfile(author, avatarJid)");
    expect(contentArea).toContain("openProfile: onAvatarClick");
  });
});
