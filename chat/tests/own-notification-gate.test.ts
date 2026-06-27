import { afterEach, describe, expect, test } from "bun:test";

import { presenceSuppressesOwnNotifications } from "../src/presence/own-notification-gate";
import {
  $ownNotificationsSuppressed,
  pickPresence,
  resetPresenceMode,
} from "../src/presence/presence-store";

describe("own-notification-gate: presenceSuppressesOwnNotifications", () => {
  test("a Do Not Disturb Show suppresses the user's own notifications", () => {
    expect(presenceSuppressesOwnNotifications("dnd")).toBe(true);
  });

  test("Available and Away Shows do not suppress own notifications", () => {
    expect(presenceSuppressesOwnNotifications("available")).toBe(false);
    expect(presenceSuppressesOwnNotifications("away")).toBe(false);
  });
});

describe("own-notification-gate: $ownNotificationsSuppressed driven by the picker", () => {
  afterEach(() => {
    resetPresenceMode();
  });

  test("picking Do Not Disturb suppresses own notifications", () => {
    pickPresence("dnd");
    expect($ownNotificationsSuppressed.get()).toBe(true);
  });

  test("Reset to automatic restores notifications", () => {
    pickPresence("dnd");
    resetPresenceMode();
    expect($ownNotificationsSuppressed.get()).toBe(false);
  });

  test("Available and Away do not suppress own notifications", () => {
    pickPresence("available");
    expect($ownNotificationsSuppressed.get()).toBe(false);
    pickPresence("away");
    expect($ownNotificationsSuppressed.get()).toBe(false);
  });

  test("the default Automatic mode does not suppress own notifications", () => {
    expect($ownNotificationsSuppressed.get()).toBe(false);
  });
});
