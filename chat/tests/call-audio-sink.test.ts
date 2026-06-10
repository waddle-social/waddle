import { describe, expect, test } from "bun:test";
import {
  CallAudioSinkAttachments,
  callAudioSinkTrackKey,
  callAudioSinkTracks,
  type CallAudioSinkTrack,
} from "../src/lib/calls/call-audio-sink";
import type { RemoteMediaTrack } from "../src/lib/calls/engine";

type FakeAttachableTrack = {
  attached: HTMLMediaElement[];
  attach: (el: HTMLMediaElement) => void;
  detach: (el: HTMLMediaElement) => HTMLMediaElement;
};

class FakeHTMLAudioElement {
  srcObject: unknown = null;
}

const globalRef = globalThis as unknown as {
  HTMLAudioElement: typeof FakeHTMLAudioElement;
  HTMLMediaElement: typeof FakeHTMLAudioElement;
};
if (typeof globalRef.HTMLAudioElement === "undefined") {
  globalRef.HTMLAudioElement = FakeHTMLAudioElement;
}
if (typeof globalRef.HTMLMediaElement === "undefined") {
  globalRef.HTMLMediaElement = FakeHTMLAudioElement;
}

function fakeAttachableTrack(): FakeAttachableTrack {
  const attached: HTMLMediaElement[] = [];
  return {
    attached,
    attach(el) {
      attached.push(el);
    },
    detach(el) {
      const index = attached.indexOf(el);
      if (index >= 0) attached.splice(index, 1);
      return el;
    },
  };
}

function fakeAudioEl(): HTMLMediaElement {
  return new FakeHTMLAudioElement() as unknown as HTMLMediaElement;
}

function remoteTrack(
  publicationSid: string,
  kind: "audio" | "video",
  source: RemoteMediaTrack["source"],
  track: unknown = fakeAttachableTrack(),
): RemoteMediaTrack {
  return {
    participantIdentity: "alice@example.com/web",
    publicationSid,
    kind,
    source,
    track: track as never,
  };
}

describe("CallAudioSink", () => {
  test("owns microphone and screen-share audio tracks", () => {
    const tracks = callAudioSinkTracks([
      remoteTrack("camera", "video", "camera"),
      remoteTrack("mic", "audio", "microphone"),
      remoteTrack("screen", "video", "screen_share"),
      remoteTrack("screen-audio", "audio", "screen_share_audio"),
    ]);

    expect(tracks.map((track) => track.publicationSid)).toEqual(["mic", "screen-audio"]);
  });

  test("keys audio elements by participant and publication", () => {
    const track = remoteTrack("mic", "audio", "microphone") as CallAudioSinkTrack;

    expect(callAudioSinkTrackKey(track)).toBe("call-audio:alice@example.com/web:mic");
  });

  test("keeps sink-owned audio attached while call surfaces unmount and remount", () => {
    const sink = new CallAudioSinkAttachments();
    const track = fakeAttachableTrack();
    const audio = remoteTrack("mic", "audio", "microphone", track) as CallAudioSinkTrack;
    const el = fakeAudioEl();

    sink.sync(audio, el);
    // Simulates navigating away from the conversation call surface:
    // no sink ref update happens, so playback remains owned here.
    expect(track.attached).toEqual([el]);
    expect(sink.size()).toBe(1);

    // Simulates returning to the conversation and remounting tiles:
    // the sink sees the same audio element/track pair and must not
    // attach a duplicate playback element.
    sink.sync(audio, el);
    expect(track.attached).toEqual([el]);
    expect(sink.size()).toBe(1);
  });

  test("detaches playback when call-end removes audio tracks", () => {
    const sink = new CallAudioSinkAttachments();
    const track = fakeAttachableTrack();
    const audio = remoteTrack("mic", "audio", "microphone", track) as CallAudioSinkTrack;
    const el = fakeAudioEl();
    el.srcObject = { stream: true };

    sink.sync(audio, el);
    sink.sync(audio, null);

    expect(track.attached).toEqual([]);
    expect(el.srcObject).toBeNull();
    expect(sink.size()).toBe(0);
  });

  test("detaches every audio element when the persistent sink unmounts", () => {
    const sink = new CallAudioSinkAttachments();
    const micTrack = fakeAttachableTrack();
    const shareTrack = fakeAttachableTrack();
    const mic = remoteTrack("mic", "audio", "microphone", micTrack) as CallAudioSinkTrack;
    const share = remoteTrack("screen-audio", "audio", "screen_share_audio", shareTrack) as CallAudioSinkTrack;
    const micEl = fakeAudioEl();
    const shareEl = fakeAudioEl();

    sink.sync(mic, micEl);
    sink.sync(share, shareEl);
    sink.detachAll();

    expect(micTrack.attached).toEqual([]);
    expect(shareTrack.attached).toEqual([]);
    expect(sink.size()).toBe(0);
  });
});
