import { describe, expect, test } from "bun:test";
import {
  TileAttachments,
  type TileAttachable,
} from "../src/lib/calls/tile-attach";

/**
 * Fake LiveKit Track that records every attach/detach so tests can
 * assert on the cumulative `attachedElements` LiveKit would track.
 * Mirrors the relevant surface of `livekit-client`'s `Track.attach` /
 * `Track.detach` so the helper's behaviour can be exercised without
 * pulling in the SDK (whose Room constructor reaches for browser
 * globals).
 */
function fakeTrack(): TileAttachable & { attached: HTMLMediaElement[] } {
  const attached: HTMLMediaElement[] = [];
  const t: TileAttachable & { attached: HTMLMediaElement[] } = {
    attached,
    attach(el) {
      attached.push(el);
    },
    detach(el) {
      const i = attached.indexOf(el);
      if (i >= 0) attached.splice(i, 1);
      return el;
    },
  };
  return t;
}

/**
 * Minimal `HTMLMediaElement`-shaped stub. The helper only touches
 * `srcObject`, and Bun's test env has no DOM, so a plain object with
 * the right shape is enough.
 */
class FakeHTMLVideoElement {
  srcObject: unknown = null;
}
class FakeHTMLAudioElement {
  srcObject: unknown = null;
}
const globalRef = globalThis as unknown as {
  HTMLVideoElement: typeof FakeHTMLVideoElement;
  HTMLAudioElement: typeof FakeHTMLAudioElement;
  HTMLMediaElement: typeof FakeHTMLVideoElement;
};
if (typeof globalRef.HTMLVideoElement === "undefined") {
  globalRef.HTMLVideoElement = FakeHTMLVideoElement;
}
if (typeof globalRef.HTMLAudioElement === "undefined") {
  globalRef.HTMLAudioElement = FakeHTMLAudioElement;
}
if (typeof globalRef.HTMLMediaElement === "undefined") {
  globalRef.HTMLMediaElement = FakeHTMLVideoElement;
}

function fakeEl(tag: "video" | "audio" = "video"): HTMLMediaElement {
  const el = tag === "video"
    ? new FakeHTMLVideoElement()
    : new FakeHTMLAudioElement();
  return el as unknown as HTMLMediaElement;
}

describe("TileAttachments", () => {
  test("mount: attaches the track to the element and remembers the pair", () => {
    const attachments = new TileAttachments();
    const el = fakeEl();
    const track = fakeTrack();
    attachments.sync("alice:video", el, track);
    expect(track.attached).toEqual([el]);
    expect(attachments.size()).toBe(1);
  });

  test("unmount (el=null) detaches the previous track and frees srcObject", () => {
    const attachments = new TileAttachments();
    const el = fakeEl();
    const track = fakeTrack();
    // simulate a srcObject the browser set on attach.
    (el as { srcObject: unknown }).srcObject = { stream: true };
    attachments.sync("alice:video", el, track);
    attachments.sync("alice:video", null, null);
    expect(track.attached).toEqual([]);
    expect(el.srcObject).toBeNull();
    expect(attachments.size()).toBe(0);
  });

  test("re-mount with the same (el, track) is a no-op", () => {
    // Vue can fire the ref callback with the same pair on a reactive
    // update; we MUST NOT call `track.attach` a second time — that's
    // exactly what causes `attachedElements` to double up.
    const attachments = new TileAttachments();
    const el = fakeEl();
    const track = fakeTrack();
    attachments.sync("alice:video", el, track);
    attachments.sync("alice:video", el, track);
    expect(track.attached).toEqual([el]);
  });

  test("focus toggle: same track swapped to a new element detaches the old el", () => {
    // Equivalent to a user clicking a tile to enter focus layout —
    // the same track is now bound to a different `<video>`. Without
    // detaching the previous element, LiveKit would attempt to drive
    // both video elements off one MediaStreamTrack.
    const attachments = new TileAttachments();
    const elA = fakeEl();
    const elB = fakeEl();
    const track = fakeTrack();
    attachments.sync("alice:video", elA, track);
    attachments.sync("alice:video", elB, track);
    expect(track.attached).toEqual([elB]);
  });

  test("participant join + leave: track detaches cleanly", () => {
    const attachments = new TileAttachments();
    const el = fakeEl();
    const track = fakeTrack();
    attachments.sync("bob:video", el, track);
    expect(track.attached).toEqual([el]);
    // Leave — Vue fires `:ref` with null because the element unmounted.
    attachments.sync("bob:video", null, null);
    expect(track.attached).toEqual([]);
    expect(attachments.size()).toBe(0);
  });

  test("repeated focus toggles never accumulate attachedElements", () => {
    // Stronger version of the regression: ten focus toggles back to
    // back should leave exactly one (el, track) attachment.
    const attachments = new TileAttachments();
    const track = fakeTrack();
    let lastEl: HTMLMediaElement | null = null;
    for (let i = 0; i < 10; i += 1) {
      const el = fakeEl();
      attachments.sync("alice:video", el, track);
      lastEl = el;
    }
    expect(track.attached).toEqual(lastEl ? [lastEl] : []);
  });
});
