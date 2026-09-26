import { afterEach, describe, expect, test } from "bun:test";
import { toast, toaster } from "../src/ui/toaster";

// zag's toast store looks a queue priority up by `type` and only knows
// error/warning/loading/success/info; a Waddle tone in that field threw
// on every `toast()` call. The tone travels in `meta` instead.
describe("toast()", () => {
  // `dismiss()` only publishes a message for the mounted Toaster to act
  // on; without one the store keeps the toast, so drop them for real.
  afterEach(() => {
    toaster.remove();
  });

  test.each([undefined, "neutral", "live", "danger"] as const)("creates a visible toast for tone %s", (tone) => {
    const id = toast({ title: "Something happened", ...(tone ? { tone } : {}) });
    expect(typeof id).toBe("string");
    expect(toaster.isVisible(id)).toBe(true);
  });

  test("carries the tone in meta and maps danger onto zag's error type", () => {
    const id = toast({ id: "shell-action-error", tone: "danger", title: "Could not send" });
    const stored = toaster.getVisibleToasts().find((item) => item.id === id);
    expect(stored?.type).toBe("error");
    expect(stored?.meta).toEqual({ tone: "danger" });
  });

  test("re-using an id updates the toast in place", () => {
    const first = toast({ id: "shell-action-error", tone: "danger", title: "First" });
    const second = toast({ id: "shell-action-error", tone: "danger", title: "Second" });
    expect(second).toBe(first);
    expect(toaster.getVisibleToasts().filter((item) => item.id === first)).toHaveLength(1);
    expect(toaster.getVisibleToasts().find((item) => item.id === first)?.title).toBe("Second");
  });
});
