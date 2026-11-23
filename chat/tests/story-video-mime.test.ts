import { describe, expect, test } from "bun:test";
import { pickStoryVideoMimeType } from "../src/lib/story-video-mime";

const supporting = (...prefixes: string[]) => (mime: string) => prefixes.some((prefix) => mime.startsWith(prefix));

describe("pickStoryVideoMimeType", () => {
  test("records MP4 with H.264 and AAC when the browser can, even if it also records WebM", () => {
    expect(pickStoryVideoMimeType(supporting("video/mp4", "video/webm"))).toBe("video/mp4;codecs=avc1.42E01E,mp4a.40.2");
  });

  test("records plain MP4 when the browser lists no codec string for it", () => {
    expect(pickStoryVideoMimeType((mime) => mime === "video/mp4")).toBe("video/mp4");
  });

  test("falls back to WebM when the browser records no MP4", () => {
    expect(pickStoryVideoMimeType(supporting("video/webm"))).toBe("video/webm;codecs=vp9,opus");
  });

  test("leaves the choice to the browser when it lists none of the formats", () => {
    expect(pickStoryVideoMimeType(() => false)).toBeUndefined();
  });
});
