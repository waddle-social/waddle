import { describe, expect, test } from "bun:test";
import {
  ACTIVE_SPEAKER_HOLD_MS,
  advanceActiveSpeakers,
  emptyActiveSpeakerState,
  evictActiveSpeaker,
  highlightedTileKeys,
} from "../src/lib/calls/active-speakers";
import { buildCallTiles } from "../src/lib/calls/call-tiles";
import type { RemoteMediaTrack } from "../src/lib/calls/engine";

const fakeTrack = {} as RemoteMediaTrack["track"];

function remoteVideo(
  participantIdentity: string,
  publicationSid: string,
  source: RemoteMediaTrack["source"],
): RemoteMediaTrack {
  return { participantIdentity, publicationSid, kind: "video", source, track: fakeTrack };
}

describe("advanceActiveSpeakers", () => {
  test("an identity LiveKit reports as speaking is highlighted immediately", () => {
    const step = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
      holdMs: 1_000,
    });

    expect([...step.activeIdentities]).toEqual(["alice@example.com/web"]);
  });

  test("a participant who stops speaking stays highlighted during the hold", () => {
    const speaking = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
      holdMs: 1_000,
    });

    const stopped = advanceActiveSpeakers({
      state: speaking.state,
      speakingIdentities: [],
      now: 500,
      holdMs: 1_000,
    });

    expect([...stopped.activeIdentities]).toEqual(["alice@example.com/web"]);
  });

  test("a held highlight persists across an intermediate step before its deadline", () => {
    const speaking = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
      holdMs: 1_000,
    });
    const stopped = advanceActiveSpeakers({
      state: speaking.state,
      speakingIdentities: [],
      now: 500,
      holdMs: 1_000,
    });

    // Another step lands before the 1_500 release deadline (e.g. a different
    // participant's event, or a sweep) — alice must stay highlighted.
    const intermediate = advanceActiveSpeakers({
      state: stopped.state,
      speakingIdentities: [],
      now: 1_200,
      holdMs: 1_000,
    });

    expect([...intermediate.activeIdentities]).toEqual(["alice@example.com/web"]);
  });

  test("the highlight clears once the hold elapses", () => {
    const speaking = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
      holdMs: 1_000,
    });
    const stopped = advanceActiveSpeakers({
      state: speaking.state,
      speakingIdentities: [],
      now: 500,
      holdMs: 1_000,
    });

    // While alice is held, the step reports when the release sweep is due.
    expect(stopped.nextDeadline).toBe(1_500);

    const cleared = advanceActiveSpeakers({
      state: stopped.state,
      speakingIdentities: [],
      now: 1_500,
      holdMs: 1_000,
    });

    expect([...cleared.activeIdentities]).toEqual([]);
    expect(cleared.nextDeadline).toBeNull();
  });

  test("highlights every simultaneously-speaking participant", () => {
    const step = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web", "bob@example.com/phone"],
      now: 0,
      holdMs: 1_000,
    });

    expect([...step.activeIdentities].sort()).toEqual([
      "alice@example.com/web",
      "bob@example.com/phone",
    ]);
  });

  test("measures the hold from when speech stops, not when it started", () => {
    // Continuous speech fires no intermediate events, so the only signals are
    // the start and the stop. The hold must run from the stop.
    const speaking = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
      holdMs: 1_000,
    });

    const stopped = advanceActiveSpeakers({
      state: speaking.state,
      speakingIdentities: [],
      now: 5_000,
      holdMs: 1_000,
    });

    expect(stopped.nextDeadline).toBe(6_000);
    expect([...stopped.activeIdentities]).toEqual(["alice@example.com/web"]);
  });

  test("a participant who resumes before the deadline keeps the highlight with no pending release", () => {
    const speaking = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
      holdMs: 1_000,
    });
    const stopped = advanceActiveSpeakers({
      state: speaking.state,
      speakingIdentities: [],
      now: 200,
      holdMs: 1_000,
    });

    const resumed = advanceActiveSpeakers({
      state: stopped.state,
      speakingIdentities: ["alice@example.com/web"],
      now: 400,
      holdMs: 1_000,
    });

    expect([...resumed.activeIdentities]).toEqual(["alice@example.com/web"]);
    expect(resumed.nextDeadline).toBeNull();
  });

  test("holds for ACTIVE_SPEAKER_HOLD_MS by default when no holdMs is given", () => {
    const speaking = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["alice@example.com/web"],
      now: 0,
    });
    const stopped = advanceActiveSpeakers({
      state: speaking.state,
      speakingIdentities: [],
      now: 0,
    });

    expect(stopped.nextDeadline).toBe(ACTIVE_SPEAKER_HOLD_MS);
  });

  test("ignores blank identities", () => {
    const step = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["", "  "],
      now: 0,
      holdMs: 1_000,
    });

    expect([...step.activeIdentities]).toEqual([]);
  });
});

describe("highlightedTileKeys", () => {
  test("an active identity highlights their camera tile", () => {
    const tiles = buildCallTiles({
      remoteTracks: [remoteVideo("alice@example.com/web", "cam", "camera")],
      localTracks: [],
      localIdentity: null,
      micEnabled: true,
    });

    const highlighted = highlightedTileKeys(tiles, new Set(["alice@example.com/web"]));

    expect([...highlighted]).toEqual(["remote:alice@example.com/web:camera"]);
  });

  test("a screen-share tile is never highlighted, even for an active speaker", () => {
    const tiles = buildCallTiles({
      remoteTracks: [
        remoteVideo("alice@example.com/web", "cam", "camera"),
        remoteVideo("alice@example.com/web", "screen", "screen_share"),
      ],
      localTracks: [],
      localIdentity: null,
      micEnabled: true,
    });

    const highlighted = highlightedTileKeys(tiles, new Set(["alice@example.com/web"]));

    expect([...highlighted]).toEqual(["remote:alice@example.com/web:camera"]);
  });

  test("the local self tile highlights when the local participant is the active speaker", () => {
    const tiles = buildCallTiles({
      remoteTracks: [],
      localTracks: [],
      localIdentity: "me@example.com/web",
      micEnabled: true,
    });

    const highlighted = highlightedTileKeys(tiles, new Set(["me@example.com/web"]));

    expect([...highlighted]).toEqual(["self:me@example.com/web:camera"]);
  });

  test("tiles for non-active identities are not highlighted", () => {
    const tiles = buildCallTiles({
      remoteTracks: [remoteVideo("alice@example.com/web", "cam", "camera")],
      localTracks: [],
      localIdentity: null,
      micEnabled: true,
    });

    const highlighted = highlightedTileKeys(tiles, new Set(["bob@example.com/phone"]));

    expect([...highlighted]).toEqual([]);
  });
});

describe("evictActiveSpeaker", () => {
  test("removes a currently-speaking identity from the derived active set", () => {
    const spoke = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["bob@example.com/web", "carol@example.com/web"],
      now: 0,
    });

    const evicted = evictActiveSpeaker(spoke.state, "bob@example.com/web");
    const next = advanceActiveSpeakers({
      state: evicted,
      speakingIdentities: ["carol@example.com/web"],
      now: 10,
    });

    expect([...next.activeIdentities]).toEqual(["carol@example.com/web"]);
  });

  test("removes an identity still inside its release hold so it cannot be re-highlighted", () => {
    // Bob speaks, then falls silent — he is now held in the release countdown.
    const spoke = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["bob@example.com/web"],
      now: 0,
    });
    const releasing = advanceActiveSpeakers({
      state: spoke.state,
      speakingIdentities: [],
      now: 100,
    });
    expect(releasing.activeIdentities.has("bob@example.com/web")).toBe(true);

    // Bob leaves mid-hold; evicting him drops the lingering release entry.
    const evicted = evictActiveSpeaker(releasing.state, "bob@example.com/web");
    const next = advanceActiveSpeakers({
      state: evicted,
      speakingIdentities: [],
      now: 200,
    });

    expect(next.activeIdentities.has("bob@example.com/web")).toBe(false);
    expect(next.nextDeadline).toBeNull();
  });

  test("returns the same state object when the identity is absent", () => {
    const spoke = advanceActiveSpeakers({
      state: emptyActiveSpeakerState(),
      speakingIdentities: ["bob@example.com/web"],
      now: 0,
    });

    expect(evictActiveSpeaker(spoke.state, "carol@example.com/web")).toBe(spoke.state);
  });
});
