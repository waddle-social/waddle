import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  createIncomingCallAlertController,
  type IncomingCallNotifier,
} from "../src/shell/audio-alerts";
import {
  $ownNotificationsSuppressed,
  pickPresence,
  resetPresenceMode,
} from "../src/presence/presence-store";

// Presence Do Not Disturb (effective `<show>dnd</show>`) must silence this
// device's own incoming-call ringtone AND OS notification banner — the whole
// alert-controller path (ADR-010 / epic #1071, follow-up #1081). The in-app
// IncomingCallToast renders from `$callState`, not from this controller, so it
// stays visible; only the disturbance (sound + OS banner) is suppressed.

function makePlayer() {
  return {
    startLoop: mock((_key: string) => {}),
    stop: mock((_key: string) => {}),
  };
}

const CALL = { peerJid: "bob@example.com", sid: "call-1", media: { audio: true, video: false } };

describe("incoming-call alert under presence Do Not Disturb", () => {
  test("does not ring the tone while effective Show is Do Not Disturb", () => {
    const player = makePlayer();
    const controller = createIncomingCallAlertController({
      player,
      isDoNotDisturb: () => true,
    });

    controller.start(CALL);

    expect(player.startLoop).not.toHaveBeenCalled();
  });

  test("does not raise the OS notification banner under DND (tab unfocused)", () => {
    const player = makePlayer();
    const notifier: IncomingCallNotifier = {
      showIncomingCall: mock(() => ({ close: () => {} })),
    };
    const controller = createIncomingCallAlertController({
      player,
      notifier,
      isTabFocused: () => false,
      isDoNotDisturb: () => true,
    });

    controller.start(CALL);

    expect(notifier.showIncomingCall).not.toHaveBeenCalled();
  });

  test("rings and banners normally when not in Do Not Disturb", () => {
    const player = makePlayer();
    const notifier: IncomingCallNotifier = {
      showIncomingCall: mock(() => ({ close: () => {} })),
    };
    const controller = createIncomingCallAlertController({
      player,
      notifier,
      isTabFocused: () => false,
      isDoNotDisturb: () => false,
    });

    controller.start(CALL);

    expect(player.startLoop).toHaveBeenCalledWith("call-1");
    expect(notifier.showIncomingCall).toHaveBeenCalledTimes(1);
  });

  test("a DND-suppressed start leaves stop and stopAll as safe no-ops", () => {
    const player = makePlayer();
    const controller = createIncomingCallAlertController({
      player,
      isDoNotDisturb: () => true,
    });

    controller.start(CALL);
    // Nothing was started, so tearing the slot down must not touch the player
    // and must not throw on an sid that was never registered as active.
    controller.stop(CALL.sid);
    controller.stopAll();

    expect(player.stop).not.toHaveBeenCalled();
  });
});

// Integration: prove the real presence-store signal — the exact expression the
// production caller wires (`ChatReadyShell.vue`: `() => $ownNotificationsSuppressed.get()`)
// — reaches the controller. Guards the seam between the picker store and the
// call alert, not just the injected-predicate contract.
describe("incoming-call alert wired to the presence store", () => {
  afterEach(() => {
    // The store is module-global; reset so a picked DND can't leak across tests.
    resetPresenceMode();
  });

  test("picking Do Not Disturb suppresses the ring; resetting restores it", () => {
    const player = makePlayer();
    const controller = createIncomingCallAlertController({
      player,
      isDoNotDisturb: () => $ownNotificationsSuppressed.get(),
    });

    pickPresence("dnd");
    controller.start(CALL);
    expect(player.startLoop).not.toHaveBeenCalled();

    resetPresenceMode();
    controller.start(CALL);
    expect(player.startLoop).toHaveBeenCalledWith("call-1");
  });
});
