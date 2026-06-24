import { describe, expect, test } from "bun:test";

import { mostAvailableShow } from "../src/presence/most-available";

describe("most-available: mostAvailableShow", () => {
  test("available on one resource and offline on another renders available", () => {
    expect(mostAvailableShow(["available", "offline"])).toBe("available");
  });

  test("no resources renders offline", () => {
    expect(mostAvailableShow([])).toBe("offline");
  });

  test("away beats offline", () => {
    expect(mostAvailableShow(["away", "offline"])).toBe("away");
  });

  test("available outranks away", () => {
    expect(mostAvailableShow(["away", "available"])).toBe("available");
  });

  test("extended away beats offline but loses to away", () => {
    expect(mostAvailableShow(["xa", "offline"])).toBe("xa");
    expect(mostAvailableShow(["away", "xa"])).toBe("away");
  });

  test("a deliberate do-not-disturb wins over any other online state", () => {
    expect(mostAvailableShow(["available", "dnd"])).toBe("dnd");
    expect(mostAvailableShow(["dnd", "away"])).toBe("dnd");
    expect(mostAvailableShow(["dnd", "offline"])).toBe("dnd");
  });
});
