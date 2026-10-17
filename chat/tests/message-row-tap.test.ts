import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { effectScope } from "vue";
import { shouldOpenThreadOnRowTap, tapTargetIsInteractive } from "../src/components/chat/message-row-tap";
import { useMessageGestures } from "../src/components/chat/composables/use-message-gestures";
import { setupVueComponent } from "./helpers/render-vue-sfc";

function pointer(type: "touch" | "mouse", x = 0, y = 0, pointerId = 1): PointerEvent {
  return { pointerId, pointerType: type, clientX: x, clientY: y, target: null, preventDefault() {} } as unknown as PointerEvent;
}

// Minimal element model: `closest` walks parents matching a tag name or
// attribute presence, which is all the tap filter needs.
interface FakeElement {
  addEventListener(): void;
  removeEventListener(): void;
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
    addEventListener() {},
    removeEventListener() {},
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
  function setup() {
    const scope = effectScope();
    const gestures = scope.run(() =>
      useMessageGestures({ onLongPress() {}, onSwipeLeft() {}, onSwipeRight() {} }),
    )!;
    return { gestures, stop: () => scope.stop() };
  }

  test("a touch that stays put is a tap, read once; a mouse click is not", () => {
    const { gestures, stop } = setup();
    expect(gestures.consumeStationaryTap()).toBe(false);
    gestures.handlers.onPointerdown(pointer("touch", 10, 10));
    gestures.handlers.onPointermove(pointer("touch", 14, 12));
    gestures.handlers.onPointerup(pointer("touch", 14, 12));
    expect(gestures.consumeStationaryTap()).toBe(true);
    expect(gestures.consumeStationaryTap()).toBe(false);
    gestures.handlers.onPointerdown(pointer("mouse", 10, 10));
    gestures.handlers.onPointerup(pointer("mouse", 10, 10));
    expect(gestures.consumeStationaryTap()).toBe(false);
    stop();
  });

  test("a swipe or a browser-taken scroll is not a tap", () => {
    const { gestures, stop } = setup();
    gestures.handlers.onPointerdown(pointer("touch", 100, 10));
    gestures.handlers.onPointermove(pointer("touch", 20, 12));
    gestures.handlers.onPointerup(pointer("touch", 20, 12));
    expect(gestures.consumeStationaryTap()).toBe(false);

    gestures.handlers.onPointerdown(pointer("touch", 10, 10));
    gestures.handlers.onPointercancel(pointer("touch", 10, 10));
    expect(gestures.consumeStationaryTap()).toBe(false);
    stop();
  });

  test("only the first touch pointer counts; a second finger voids the tap", () => {
    const { gestures, stop } = setup();
    gestures.handlers.onPointerdown(pointer("touch", 10, 10, 1));
    gestures.handlers.onPointerdown(pointer("touch", 80, 10, 2));
    gestures.handlers.onPointerup(pointer("touch", 80, 10, 2));
    gestures.handlers.onPointerup(pointer("touch", 10, 10, 1));
    expect(gestures.consumeStationaryTap()).toBe(false);

    // An untracked pointer's release never decides the tap.
    gestures.handlers.onPointerdown(pointer("touch", 10, 10, 3));
    gestures.handlers.onPointerup(pointer("touch", 10, 10, 9));
    expect(gestures.consumeStationaryTap()).toBe(false);
    gestures.handlers.onPointerup(pointer("touch", 10, 10, 3));
    expect(gestures.consumeStationaryTap()).toBe(true);
    stop();
  });
});

describe("MessageCard row tap", () => {
  const originalWindow = globalThis.window;
  const originalDocument = globalThis.document;
  let coarse = true;
  let selectedText = "";

  beforeEach(() => {
    coarse = true;
    selectedText = "";
  });

  afterEach(() => {
    (globalThis as { window?: unknown }).window = originalWindow;
    (globalThis as { document?: unknown }).document = originalDocument;
  });

  // Browser globals only while the row's click handler runs: module
  // imports during setup must see the plain test environment.
  function withPointerEnv(run: () => void) {
    (globalThis as { window?: unknown }).window = {
      matchMedia: () => ({ matches: coarse }),
      addEventListener() {},
      removeEventListener() {},
    };
    (globalThis as { document?: unknown }).document = {
      getSelection: () => ({ isCollapsed: selectedText === "", toString: () => selectedText }),
    };
    try {
      run();
    } finally {
      (globalThis as { window?: unknown }).window = originalWindow;
      (globalThis as { document?: unknown }).document = originalDocument;
    }
  }

  async function mountRow(props: Record<string, unknown> = {}) {
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent(
      "../src/components/chat/MessageCard.vue",
      {
        message: {
          id: "root",
          author: "rawkode",
          body: "hello",
          createdAt: "2026-10-17T10:00:00Z",
          createdAtSource: "archive",
          isSelf: false,
        },
        hats: [],
        threadReplyCount: 2,
        ...props,
      },
      import.meta.url,
      (...args) => emitted.push(args),
    );
    const row = el("div");
    const body = el("p", el("div", row));
    const link = el("a", body, { href: "https://example.com" });
    (bindings.bubbleEl as { value: unknown }).value = row;
    const gestures = bindings.gestures as ReturnType<typeof useMessageGestures>;
    const rowClick = bindings.onRowClick as (event: MouseEvent) => void;
    const click = (event: MouseEvent) => withPointerEnv(() => rowClick(event));
    const touchTap = (target: unknown) => {
      gestures.handlers.onPointerdown(pointer("touch", 10, 10));
      gestures.handlers.onPointerup(pointer("touch", 10, 10));
      click({ target } as unknown as MouseEvent);
    };
    const opened = () => emitted.filter(([name]) => name === "openThread").map(([, id]) => id);
    return { gestures, click, touchTap, opened, body, link };
  }

  test("a stationary coarse touch tap on post text opens its thread", async () => {
    const row = await mountRow();
    row.touchTap(row.body);
    expect(row.opened()).toEqual(["root"]);
  }, 30_000);

  test("interactive targets, fine pointers, selections and the thread pane do not", async () => {
    const row = await mountRow();
    row.touchTap(row.link);
    coarse = false;
    row.touchTap(row.body);
    coarse = true;
    selectedText = "hello";
    row.touchTap(row.body);
    expect(row.opened()).toEqual([]);

    selectedText = "";
    const paneRow = await mountRow({ hideThreadChip: true });
    paneRow.touchTap(paneRow.body);
    expect(paneRow.opened()).toEqual([]);
  }, 30_000);

  test("a tap that only stops a scroll gets no click and leaves no stale tap", async () => {
    const row = await mountRow();
    row.gestures.handlers.onPointerdown(pointer("touch", 10, 10));
    row.gestures.handlers.onPointerup(pointer("touch", 10, 10));
    // No click follows; the next press is a mouse click on the text.
    row.gestures.handlers.onPointerdown(pointer("mouse", 10, 10));
    row.gestures.handlers.onPointerup(pointer("mouse", 10, 10));
    row.click({ target: row.body } as unknown as MouseEvent);
    expect(row.opened()).toEqual([]);
  }, 30_000);

  test("a long press opens the menu, not the thread", async () => {
    const row = await mountRow();
    row.gestures.handlers.onPointerdown(pointer("touch", 10, 10));
    await new Promise((resolve) => setTimeout(resolve, 700));
    row.gestures.handlers.onPointerup(pointer("touch", 10, 10));
    row.click({ target: row.body } as unknown as MouseEvent);
    expect(row.opened()).toEqual([]);
  }, 30_000);
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

  test("an anonymous occupant's profile opens but can never be messaged", async () => {
    const emitted: unknown[][] = [];
    const open = { value: true };
    const anonymous = await setupVueComponent(
      "../src/components/chat/UserProfileDrawer.vue",
      { username: "guest", jid: "", avatarJid: null, open: open.value },
      import.meta.url,
      (...args) => emitted.push(args),
    );
    expect((anonymous.canMessage as { value: boolean }).value).toBe(false);
    (anonymous.messagePerson as () => void)();
    expect(emitted.filter(([name]) => name === "message")).toEqual([]);

    const known = await setupVueComponent(
      "../src/components/chat/UserProfileDrawer.vue",
      { username: "alice", jid: "alice@example.com", avatarJid: "alice@example.com", open: true },
      import.meta.url,
      (...args) => emitted.push(args),
    );
    expect((known.canMessage as { value: boolean }).value).toBe(true);
    (known.messagePerson as () => void)();
    expect(emitted.filter(([name]) => name === "message")).toHaveLength(1);
  }, 30_000);
});
