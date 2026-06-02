import { describe, expect, test } from "bun:test";
import {
  shouldPreserveActiveChannelDuringStructureRetry,
  shouldRetryMissingStructureLoad,
  type MissingStructureRetryInput,
} from "../src/shell/structure-retry";

describe("missing structure reconnect retry", () => {
  test("retries once after the initial directory load finishes in an online epoch", () => {
    expect(shouldRetryMissingStructureLoad(input())).toBe(true);
  });

  test("does not run before the initial directory load has finished", () => {
    expect(shouldRetryMissingStructureLoad(input({ initialLoadFinished: false }))).toBe(false);
  });

  test("retries when channels are missing", () => {
    expect(shouldRetryMissingStructureLoad(input({ spaceCount: 1, channelCount: 0 }))).toBe(true);
  });

  test("retries when spaces are missing but channels are present", () => {
    expect(shouldRetryMissingStructureLoad(input({ spaceCount: 0, channelCount: 2 }))).toBe(true);
  });

  test("retries when the current channel route target is missing", () => {
    expect(shouldRetryMissingStructureLoad(input({ channelCount: 2, routeTargetMissing: true }))).toBe(true);
  });

  test("does not retry when spaces and channels are discovered and the current route is resolved", () => {
    expect(shouldRetryMissingStructureLoad(input({ spaceCount: 1, channelCount: 1 }))).toBe(false);
  });

  test("does not retry while another structure load is active", () => {
    expect(shouldRetryMissingStructureLoad(input({ isLoadingStructure: true }))).toBe(false);
    expect(shouldRetryMissingStructureLoad(input({ inFlight: true }))).toBe(false);
  });

  test("does not retry again within the same online period", () => {
    expect(shouldRetryMissingStructureLoad(input({ lastAttemptedOnlineEpoch: 2 }))).toBe(false);
  });
});

function input(overrides: Partial<MissingStructureRetryInput> = {}): MissingStructureRetryInput {
  return {
    appReady: true,
    hasClient: true,
    initialLoadFinished: true,
    inFlight: false,
    isLoadingStructure: false,
    spaceCount: 0,
    channelCount: 0,
    routeTargetMissing: false,
    xmppStatus: "online",
    onlineEpoch: 2,
    lastAttemptedOnlineEpoch: 1,
    ...overrides,
  };
}

describe("structure retry channel preservation", () => {
  test("preserves a listed active channel when no unresolved route is pending", () => {
    expect(shouldPreserveActiveChannelDuringStructureRetry({
      activeChannelListed: true,
      routeTargetMissing: false,
    })).toBe(true);
  });

  test("uses no-select reloads for pending route recovery or stale active channels", () => {
    expect(shouldPreserveActiveChannelDuringStructureRetry({
      activeChannelListed: true,
      routeTargetMissing: true,
    })).toBe(false);
    expect(shouldPreserveActiveChannelDuringStructureRetry({
      activeChannelListed: false,
      routeTargetMissing: false,
    })).toBe(false);
  });
});
