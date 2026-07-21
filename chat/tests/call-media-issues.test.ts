import { afterEach, describe, expect, test } from "bun:test";
import {
  $callMediaIssues,
  classifyMediaError,
  clearAllMediaIssues,
  clearMediaIssue,
  mediaErrorMessage,
  mediaIssueMessage,
  recordMediaIssue,
} from "../src/lib/calls/call-media-issues";
import { __setFaroForTesting } from "../src/lib/telemetry";

function domError(name: string): Error {
  const err = new Error(`${name} message`);
  err.name = name;
  return err;
}

afterEach(() => {
  clearAllMediaIssues();
  __setFaroForTesting(null);
});

describe("classifyMediaError", () => {
  test("maps permission rejections (incl. Chrome legacy aliases) to 'denied'", () => {
    expect(classifyMediaError(domError("NotAllowedError"))).toBe("denied");
    expect(classifyMediaError(domError("PermissionDeniedError"))).toBe("denied");
    expect(classifyMediaError(domError("SecurityError"))).toBe("denied");
  });

  test("maps missing-device rejections to 'missing'", () => {
    expect(classifyMediaError(domError("NotFoundError"))).toBe("missing");
    expect(classifyMediaError(domError("DevicesNotFoundError"))).toBe("missing");
    expect(classifyMediaError(domError("OverconstrainedError"))).toBe("missing");
  });

  test("maps busy-device rejections to 'in-use'", () => {
    expect(classifyMediaError(domError("NotReadableError"))).toBe("in-use");
    expect(classifyMediaError(domError("TrackStartError"))).toBe("in-use");
    expect(classifyMediaError(domError("AbortError"))).toBe("in-use");
  });

  test("falls back to 'failed' for unknown / non-error values", () => {
    expect(classifyMediaError(domError("WeirdError"))).toBe("failed");
    expect(classifyMediaError(new Error("plain"))).toBe("failed");
    expect(classifyMediaError("nope")).toBe("failed");
    expect(classifyMediaError(null)).toBe("failed");
  });
});

describe("mediaErrorMessage (generic toast mapping)", () => {
  test("returns friendly copy for media DOMExceptions", () => {
    expect(mediaErrorMessage(domError("NotAllowedError"))).toContain("blocked");
    expect(mediaErrorMessage(domError("NotFoundError"))).toContain("No camera or microphone");
  });

  test("returns null for non-media errors so the caller keeps the raw message", () => {
    expect(mediaErrorMessage(new Error("xmpp timeout"))).toBeNull();
    expect(mediaErrorMessage("boom")).toBeNull();
  });
});

describe("mediaIssueMessage (notice copy)", () => {
  test("is kind-specific", () => {
    expect(mediaIssueMessage("mic", "missing")).toContain("microphone");
    expect(mediaIssueMessage("cam", "missing")).toContain("camera");
    expect(mediaIssueMessage("screen", "failed")).toContain("screen sharing");
    expect(mediaIssueMessage("mic", "denied")).toContain("listener");
  });
});

describe("$callMediaIssues store", () => {
  test("reports permission and missing-device failures as recoverable Faro errors", () => {
    const errors: Array<{
      error: Error;
      options: { type?: string; context?: Record<string, string> };
    }> = [];
    __setFaroForTesting({
      api: {
        pushError: (error: Error, options?: { type?: string; context?: Record<string, string> }) => {
          errors.push({ error, options: options ?? {} });
        },
      },
    } as never);

    recordMediaIssue("mic", domError("NotAllowedError"));
    recordMediaIssue("cam", domError("NotFoundError"));

    expect(errors.map(({ options }) => options)).toEqual([
      {
        type: "call.media",
        context: expect.objectContaining({
          kind: "call.media",
          recoverable: "true",
          media_kind: "mic",
          reason: "denied",
        }),
      },
      {
        type: "call.media",
        context: expect.objectContaining({
          kind: "call.media",
          recoverable: "true",
          media_kind: "cam",
          reason: "missing",
        }),
      },
    ]);
    expect(errors.map(({ error }) => error.message)).toEqual([
      "call.media.mic.denied",
      "call.media.cam.missing",
    ]);
    expect(errors.map(({ error }) => error.message).join(" ")).not.toContain("NotAllowedError message");
  });

  test("record/clear a single kind without touching the other", () => {
    recordMediaIssue("mic", domError("NotFoundError"));
    expect($callMediaIssues.get()).toEqual({ mic: "missing", cam: null, screen: null });

    recordMediaIssue("cam", domError("NotAllowedError"));
    expect($callMediaIssues.get()).toEqual({ mic: "missing", cam: "denied", screen: null });

    recordMediaIssue("screen", domError("NotReadableError"));
    expect($callMediaIssues.get()).toEqual({
      mic: "missing",
      cam: "denied",
      screen: "in-use",
    });

    clearMediaIssue("mic");
    expect($callMediaIssues.get()).toEqual({ mic: null, cam: "denied", screen: "in-use" });
  });

  test("clearAllMediaIssues resets every kind", () => {
    recordMediaIssue("mic", domError("NotReadableError"));
    recordMediaIssue("cam", domError("NotFoundError"));
    recordMediaIssue("screen", domError("AbortError"));
    clearAllMediaIssues();
    expect($callMediaIssues.get()).toEqual({ mic: null, cam: null, screen: null });
  });
});
