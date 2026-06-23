import { describe, expect, mock, test } from "bun:test";
import {
  installCallShortcuts,
  type CallShortcutHandlers,
} from "../src/lib/calls/use-call-shortcuts";

/** Minimal window double that records capture listeners and replays events. */
function fakeWindow() {
  const listeners = new Map<string, Set<EventListener>>();
  return {
    addEventListener(type: string, fn: EventListener) {
      (listeners.get(type) ?? listeners.set(type, new Set()).get(type)!).add(fn);
    },
    removeEventListener(type: string, fn: EventListener) {
      listeners.get(type)?.delete(fn);
    },
    dispatch(event: { type: string }) {
      for (const fn of listeners.get(event.type) ?? []) fn(event as unknown as Event);
    },
    count(type: string) {
      return listeners.get(type)?.size ?? 0;
    },
  };
}

/** A keyboard event whose target reports membership via `closest`. */
function keyEvent(
  overrides: { type?: string; key?: string; repeat?: boolean; targetMatches?: string[] } = {},
) {
  const matches = overrides.targetMatches ?? [".call-split"];
  return {
    type: overrides.type ?? "keydown",
    key: overrides.key ?? "m",
    repeat: overrides.repeat ?? false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    preventDefault: mock(() => undefined),
    target: {
      closest(selectors: string): Element | null {
        const wanted = selectors.split(",").map((part) => part.trim());
        return wanted.some((selector) => matches.includes(selector))
          ? ({} as Element)
          : null;
      },
    },
  };
}

function handlers(): CallShortcutHandlers & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    "toggle-mic": () => calls.push("toggle-mic"),
    "push-to-talk-start": () => calls.push("ptt-start"),
    "push-to-talk-end": () => calls.push("ptt-end"),
  };
}

describe("installCallShortcuts", () => {
  test("dispatches the mapped intent and consumes the event", () => {
    const win = fakeWindow();
    const h = handlers();
    installCallShortcuts(h, win);

    const event = keyEvent({ key: "m" });
    win.dispatch(event);

    expect(h.calls).toEqual(["toggle-mic"]);
    expect(event.preventDefault).toHaveBeenCalled();
  });

  test("routes Space keydown/keyup to push-to-talk start/end", () => {
    const win = fakeWindow();
    const h = handlers();
    installCallShortcuts(h, win);

    win.dispatch(keyEvent({ key: " ", type: "keydown" }));
    win.dispatch(keyEvent({ key: " ", type: "keyup" }));

    expect(h.calls).toEqual(["ptt-start", "ptt-end"]);
  });

  test("stays inert while typing and never consumes the keystroke", () => {
    const win = fakeWindow();
    const h = handlers();
    installCallShortcuts(h, win);

    const event = keyEvent({ key: "m", targetMatches: ["textarea"] });
    win.dispatch(event);

    expect(h.calls).toEqual([]);
    expect(event.preventDefault).not.toHaveBeenCalled();
  });

  test("ignores keys with no registered handler so existing handlers run", () => {
    const win = fakeWindow();
    const h = handlers(); // no "exit-immersive" handler
    installCallShortcuts(h, win);

    const event = keyEvent({ key: "Escape" });
    win.dispatch(event);

    expect(h.calls).toEqual([]);
    expect(event.preventDefault).not.toHaveBeenCalled();
  });

  test("does not dispatch while the surface is inactive", () => {
    // Both surfaces stay mounted at once; only the visible one may act.
    const win = fakeWindow();
    const h = handlers();
    installCallShortcuts(h, win, () => false);

    const event = keyEvent({ key: "m" });
    win.dispatch(event);

    expect(h.calls).toEqual([]);
    expect(event.preventDefault).not.toHaveBeenCalled();
  });

  test("a handler returning false leaves the event for other listeners", () => {
    const win = fakeWindow();
    const calls: string[] = [];
    const h: CallShortcutHandlers = {
      "exit-immersive": () => {
        calls.push("esc");
        return false;
      },
    };
    installCallShortcuts(h, win);

    const event = keyEvent({ key: "Escape" });
    win.dispatch(event);

    expect(calls).toEqual(["esc"]);
    expect(event.preventDefault).not.toHaveBeenCalled();
  });

  test("cleanup removes both capture listeners", () => {
    const win = fakeWindow();
    const h = handlers();
    const cleanup = installCallShortcuts(h, win);
    expect(win.count("keydown")).toBe(1);
    expect(win.count("keyup")).toBe(1);

    cleanup();
    win.dispatch(keyEvent({ key: "m" }));

    expect(h.calls).toEqual([]);
    expect(win.count("keydown")).toBe(0);
    expect(win.count("keyup")).toBe(0);
  });
});
