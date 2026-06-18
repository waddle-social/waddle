import { afterEach, describe, expect, test } from "bun:test";
import {
  $callActiveSince,
  callElapsedMs,
  formatCallDuration,
  resetCallActiveSince,
  setCallActiveSince,
} from "../src/lib/calls/call-duration";

afterEach(() => {
  resetCallActiveSince();
});

describe("formatCallDuration — elapsed call timer label", () => {
  test("formats sub-minute durations as M:SS with a leading zero on seconds", () => {
    expect(formatCallDuration(5_000)).toBe("0:05");
  });

  test("rolls into minutes past 60 seconds", () => {
    expect(formatCallDuration(65_000)).toBe("1:05");
  });

  test("switches to H:MM:SS once it crosses an hour", () => {
    // 1h 01m 01s
    expect(formatCallDuration(3_661_000)).toBe("1:01:01");
  });

  test("a fresh (zero) duration reads 0:00, not blank", () => {
    expect(formatCallDuration(0)).toBe("0:00");
  });

  test("clamps a negative elapsed (clock skew) to 0:00 rather than going backwards", () => {
    expect(formatCallDuration(-5_000)).toBe("0:00");
  });

  test("floors sub-second remainders so the timer never shows a partial second", () => {
    expect(formatCallDuration(900)).toBe("0:00");
    expect(formatCallDuration(1_900)).toBe("0:01");
  });
});

describe("$callActiveSince — when the live call clock started", () => {
  test("starts unset so a header before connect shows no running timer", () => {
    expect($callActiveSince.get()).toBeNull();
  });

  test("setCallActiveSince stamps the connect instant; reset clears it", () => {
    setCallActiveSince(1_000);
    expect($callActiveSince.get()).toBe(1_000);
    resetCallActiveSince();
    expect($callActiveSince.get()).toBeNull();
  });
});

describe("callElapsedMs — elapsed since the call clock started", () => {
  test("is zero while the clock is unset (still connecting)", () => {
    expect(callElapsedMs(null, 10_000)).toBe(0);
  });

  test("is the difference between now and the stamped start", () => {
    expect(callElapsedMs(1_000, 6_500)).toBe(5_500);
  });

  test("clamps to zero if now precedes the start (clock skew)", () => {
    expect(callElapsedMs(6_500, 1_000)).toBe(0);
  });
});
