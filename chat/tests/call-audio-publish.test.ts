import { describe, expect, test } from "bun:test";
import { audioPublishOptions } from "../src/lib/calls/audio-publish";

describe("audioPublishOptions — default Opus voice-clarity profile", () => {
  test("requests ~64k mono Opus (musicHighQuality-class)", () => {
    const publish = audioPublishOptions();
    expect(publish.audioPreset?.maxBitrate).toBe(64_000);
  });

  test("keeps RED (redundant audio) and DTX (discontinuous transmission) enabled", () => {
    const publish = audioPublishOptions();
    expect(publish.red).toBe(true);
    expect(publish.dtx).toBe(true);
  });

  test("publishes mono — never lets a stereo capture inflate the bitrate or disable DTX/RED", () => {
    // DTX/RED only auto-apply to mono in LiveKit, and the target is 64k MONO,
    // not 64k stereo; pin forceStereo off so a stereo capture device can't
    // silently double the channel count.
    const publish = audioPublishOptions();
    expect(publish.forceStereo).toBe(false);
  });

  test("pure: returns fresh objects callers can't alias into a shared singleton", () => {
    const a = audioPublishOptions();
    const b = audioPublishOptions();
    expect(a).not.toBe(b);
    expect(a.audioPreset).not.toBe(b.audioPreset);
  });
});
