import { describe, expect, test } from "bun:test";
import {
  TileAttachments,
  type TileAttachable,
} from "../src/lib/calls/tile-attach";
import {
  createCallVideoRegistry,
  type CallVideoRegistry,
} from "../src/lib/calls/video-registry";

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
 *
 * For the video-vs-audio branch the helper does an
 * `instanceof HTMLVideoElement` check. Bun's test runtime doesn't
 * expose those globals, so we install lightweight class stubs once
 * per process and build instances of them.
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
    attachments.sync("alice:video", el, track, "alice@waddle.test/web", null);
    expect(track.attached).toEqual([el]);
    expect(attachments.size()).toBe(1);
  });

  test("unmount (el=null) detaches the previous track and frees srcObject", () => {
    const attachments = new TileAttachments();
    const el = fakeEl();
    const track = fakeTrack();
    // simulate a srcObject the browser set on attach.
    (el as { srcObject: unknown }).srcObject = { stream: true };
    attachments.sync("alice:video", el, track, "alice@waddle.test/web", null);
    attachments.sync("alice:video", null, null, "alice@waddle.test/web", null);
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
    attachments.sync("alice:video", el, track, "alice@waddle.test/web", null);
    attachments.sync("alice:video", el, track, "alice@waddle.test/web", null);
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
    attachments.sync("alice:video", elA, track, "alice@waddle.test/web", null);
    attachments.sync("alice:video", elB, track, "alice@waddle.test/web", null);
    expect(track.attached).toEqual([elB]);
  });

  test("participant join + leave: track detaches and registry slot clears", () => {
    // The reported scenario from the task: a participant joining,
    // their tile mounting, then leaving — must NOT leave their track
    // attached. We assert through the fake track's `attached` array.
    const attachments = new TileAttachments();
    const reg: CallVideoRegistry = createCallVideoRegistry();
    const el = fakeEl();
    const track = fakeTrack();
    attachments.sync("bob:video", el, track, "bob@waddle.test/web", reg);
    expect(reg.get("bob@waddle.test/web")).toBe(el as HTMLVideoElement);
    // Leave — Vue fires `:ref` with null because the element unmounted.
    attachments.sync("bob:video", null, null, "bob@waddle.test/web", reg);
    expect(track.attached).toEqual([]);
    expect(reg.get("bob@waddle.test/web")).toBeNull();
  });

  test("repeated focus toggles never accumulate attachedElements", () => {
    // Stronger version of the regression: ten focus toggles back to
    // back should leave exactly one (el, track) attachment.
    const attachments = new TileAttachments();
    const track = fakeTrack();
    let lastEl: HTMLMediaElement | null = null;
    for (let i = 0; i < 10; i += 1) {
      const el = fakeEl();
      attachments.sync("alice:video", el, track, "alice@waddle.test/web", null);
      lastEl = el;
    }
    expect(track.attached).toEqual(lastEl ? [lastEl] : []);
  });

  test("identity is only registered for HTMLVideoElement, not audio", () => {
    // The PIP registry is for video tiles; audio attachments must
    // not register into it (they'd be invalid PIP targets).
    const attachments = new TileAttachments();
    const reg = createCallVideoRegistry();
    const audioEl = fakeEl("audio");
    const track = fakeTrack();
    attachments.sync("alice:audio", audioEl, track, "alice@waddle.test/web", reg);
    expect(reg.get("alice@waddle.test/web")).toBeNull();
  });
});
