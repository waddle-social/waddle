import { describe, expect, mock, test } from "bun:test";
import {
  ROOM_JOIN_RETRY_BASE_DELAY_MS,
  ROOM_JOIN_RETRY_MAX_DELAY_MS,
  RoomJoinRetryCoordinator,
  roomJoinRetryDelayMs,
} from "../src/lib/xmpp/room-join-retry";
import { ManualRoomJoinRetryTimer } from "./helpers/manual-room-join-retry-timer";

describe("room join retry backoff", () => {
  test("uses exponential delays with equal jitter and a hard cap", () => {
    expect(roomJoinRetryDelayMs(0, () => 0)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS);
    expect(roomJoinRetryDelayMs(0, () => 1)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS * 2);
    expect(roomJoinRetryDelayMs(1, () => 1)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS * 4);
    expect(roomJoinRetryDelayMs(3, () => 1)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS * 16);
    expect(roomJoinRetryDelayMs(4, () => 1)).toBe(ROOM_JOIN_RETRY_MAX_DELAY_MS);
    expect(roomJoinRetryDelayMs(50, () => 1)).toBe(ROOM_JOIN_RETRY_MAX_DELAY_MS);
    expect(roomJoinRetryDelayMs(50, () => 0)).toBe(ROOM_JOIN_RETRY_MAX_DELAY_MS / 2);
  });

  test("coalesces listeners onto one scheduled retry", async () => {
    const timer = new ManualRoomJoinRetryTimer();
    const coordinator = new RoomJoinRetryCoordinator({
      timer,
      random: () => 1,
    });
    const retry = mock(async () => undefined);

    const first = coordinator.schedule("busy@muc.example.com", {
      isEligible: () => true,
      retry,
    });
    const second = coordinator.schedule("busy@muc.example.com", {
      isEligible: () => true,
      retry,
    });

    expect(second).toBe(first);
    expect(timer.scheduledDelays).toEqual([ROOM_JOIN_RETRY_BASE_DELAY_MS * 2]);

    timer.runNext();
    await first;

    expect(retry).toHaveBeenCalledTimes(1);
    expect(coordinator.pending("busy@muc.example.com")).toBeUndefined();
  });

  test("cancellation clears the timer and rejects listeners without running a retry", async () => {
    const timer = new ManualRoomJoinRetryTimer();
    const coordinator = new RoomJoinRetryCoordinator({
      timer,
      random: () => 1,
    });
    const retry = mock(async () => undefined);
    const scheduled = coordinator.schedule("busy@muc.example.com", {
      isEligible: () => true,
      retry,
    });

    coordinator.cancel("busy@muc.example.com");

    expect(timer.pendingCount).toBe(0);
    await expect(scheduled).rejects.toThrow("Room join retry cancelled");
    expect(retry).not.toHaveBeenCalled();
  });
});
