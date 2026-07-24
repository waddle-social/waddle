import { describe, expect, mock, test } from "bun:test";
import {
  ROOM_JOIN_RETRY_BASE_DELAY_MS,
  ROOM_JOIN_RETRY_MAX_DELAY_MS,
  RoomJoinRetryCoordinator,
  roomJoinRetryDelayMs,
  type RoomJoinRetryTimer,
} from "../src/lib/xmpp/room-join-retry";

class ManualRetryTimer implements RoomJoinRetryTimer {
  readonly scheduledDelays: number[] = [];
  private nextId = 1;
  private readonly callbacks = new Map<number, () => void>();

  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout> {
    const id = this.nextId++;
    this.scheduledDelays.push(delayMs);
    this.callbacks.set(id, callback);
    return id as unknown as ReturnType<typeof setTimeout>;
  }

  clearTimeout(handle: ReturnType<typeof setTimeout>): void {
    this.callbacks.delete(handle as unknown as number);
  }

  runNext(): void {
    const next = this.callbacks.entries().next().value as [number, () => void] | undefined;
    if (!next) throw new Error("No retry timer is scheduled");
    const [id, callback] = next;
    this.callbacks.delete(id);
    callback();
  }

  get pendingCount(): number {
    return this.callbacks.size;
  }
}

describe("room join retry backoff", () => {
  test("uses exponential delays with equal jitter and a hard cap", () => {
    expect(roomJoinRetryDelayMs(0, () => 0)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS / 2);
    expect(roomJoinRetryDelayMs(0, () => 1)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS);
    expect(roomJoinRetryDelayMs(1, () => 1)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS * 2);
    expect(roomJoinRetryDelayMs(4, () => 1)).toBe(ROOM_JOIN_RETRY_BASE_DELAY_MS * 16);
    expect(roomJoinRetryDelayMs(5, () => 1)).toBe(ROOM_JOIN_RETRY_MAX_DELAY_MS);
    expect(roomJoinRetryDelayMs(50, () => 1)).toBe(ROOM_JOIN_RETRY_MAX_DELAY_MS);
    expect(roomJoinRetryDelayMs(50, () => 0)).toBe(ROOM_JOIN_RETRY_MAX_DELAY_MS / 2);
  });

  test("coalesces listeners onto one scheduled retry", async () => {
    const timer = new ManualRetryTimer();
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
    expect(timer.scheduledDelays).toEqual([ROOM_JOIN_RETRY_BASE_DELAY_MS]);

    timer.runNext();
    await first;

    expect(retry).toHaveBeenCalledTimes(1);
    expect(coordinator.pending("busy@muc.example.com")).toBeUndefined();
  });

  test("cancellation clears the timer and rejects listeners without running a retry", async () => {
    const timer = new ManualRetryTimer();
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
