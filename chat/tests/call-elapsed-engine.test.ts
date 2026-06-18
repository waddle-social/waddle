import { afterEach, describe, expect, test } from "bun:test";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import {
  $callActiveSince,
  resetCallActiveSince,
} from "../src/lib/calls/call-duration";

/**
 * The stage-header's elapsed timer is driven by `$callActiveSince`, which
 * the engine lifecycle owns: stamped when the LiveKit room connects,
 * cleared when it disconnects. These tests exercise the wiring through the
 * engine's own event emitter so a future refactor of the handler can't
 * silently drop the timer.
 */

// `useCallEngine()` registers the singleton handlers on first call.
const engine = useCallEngine().engine;
const emit = (
  engine as unknown as { emit: (event: string, ...args: unknown[]) => void }
).emit.bind(engine);

afterEach(() => {
  // Balance any media-path poll the synthetic `connected` started, then
  // clear the clock for the next test.
  emit("disconnected", "local");
  resetCallActiveSince();
});

describe("call elapsed clock — engine lifecycle wiring", () => {
  test("stamps $callActiveSince when the room connects", () => {
    expect($callActiveSince.get()).toBeNull();
    emit("connected", { localIdentity: "me@waddle.test/web", remoteIdentities: [] });
    expect($callActiveSince.get()).not.toBeNull();
  });

  test("clears $callActiveSince when the room disconnects", () => {
    emit("connected", { localIdentity: "me@waddle.test/web", remoteIdentities: [] });
    expect($callActiveSince.get()).not.toBeNull();
    emit("disconnected", "local");
    expect($callActiveSince.get()).toBeNull();
  });
});
