import { describe, expect, test } from "bun:test";
import {
  advanceSpeakerPromotion,
  emptySpeakerPromotion,
} from "../src/lib/calls/view-mode";

describe("speaker promotion", () => {
  test("promotes the sole active speaker from an empty start", () => {
    const next = advanceSpeakerPromotion({
      state: emptySpeakerPromotion(),
      activeIdentities: new Set(["bob@example.com/web"]),
    });

    expect(next.promotedIdentity).toBe("bob@example.com/web");
  });

  test("keeps the current promotion while it is still active and others join in", () => {
    const next = advanceSpeakerPromotion({
      state: { promotedIdentity: "bob@example.com/web" },
      activeIdentities: new Set(["carol@example.com/web", "bob@example.com/web"]),
    });

    expect(next.promotedIdentity).toBe("bob@example.com/web");
  });

  test("holds the last speaker through a silence so the large tile never goes blank", () => {
    const next = advanceSpeakerPromotion({
      state: { promotedIdentity: "bob@example.com/web" },
      activeIdentities: new Set(),
    });

    expect(next.promotedIdentity).toBe("bob@example.com/web");
  });

  test("promotes the new speaker once the previous one is no longer active", () => {
    const next = advanceSpeakerPromotion({
      state: { promotedIdentity: "bob@example.com/web" },
      activeIdentities: new Set(["carol@example.com/web"]),
    });

    expect(next.promotedIdentity).toBe("carol@example.com/web");
  });

  test("stays empty before anyone has spoken", () => {
    const next = advanceSpeakerPromotion({
      state: emptySpeakerPromotion(),
      activeIdentities: new Set(),
    });

    expect(next.promotedIdentity).toBeNull();
  });
});
