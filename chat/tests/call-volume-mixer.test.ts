import { afterEach, describe, expect, test } from "bun:test";
import { computed, effectScope } from "vue";
import {
  buildCallVolumeMixerRows,
  callVolumePercentToGain,
  resetCallVolumeMixerLevels,
  type CallVolumeLevelStore,
  type CallVolumeMixerRow,
} from "../src/lib/calls/call-volume-mixer";
import { useCallVolumeMixer } from "../src/lib/calls/use-call-volume-mixer";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import { $callState, clearCallState } from "../src/lib/calls/call-store";

const fakeTrack = {} as never;

describe("call volume mixer projection", () => {
  test("groups each remote participant's voice before screen-share audio and excludes self", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: [
        "carol@waddle.test/tablet",
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
      ],
      remoteTracks: [
        remoteAudio("bob-mic", "bob@waddle.test/desktop", "microphone"),
        remoteAudio("bob-screen", "bob@waddle.test/desktop", "screen_share_audio"),
        remoteAudio("alice-screen", "alice@waddle.test/web", "screen_share_audio"),
        remoteAudio("self-mic", "me@waddle.test/browser", "microphone"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/desktop:microphone": 0.4,
        "bob@waddle.test/desktop:screen_share_audio": 0.7,
        "alice@waddle.test/web:screen_share_audio": 0,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      label: row.label,
      source: row.source,
      level: row.level,
      disabled: row.disabled,
      hint: row.hint,
      muted: row.muted,
    }))).toEqual([
      {
        key: "carol@waddle.test/tablet:microphone",
        label: "carol",
        source: "microphone",
        level: 1,
        disabled: true,
        hint: "mic off",
        muted: false,
      },
      {
        key: "alice@waddle.test/web:microphone",
        label: "alice",
        source: "microphone",
        level: 1,
        disabled: true,
        hint: "mic off",
        muted: false,
      },
      {
        key: "alice@waddle.test/web:screen_share_audio",
        label: "alice's screen",
        source: "screen_share_audio",
        level: 0,
        disabled: false,
        hint: null,
        muted: true,
      },
      {
        key: "bob@waddle.test/desktop:microphone",
        label: "bob",
        source: "microphone",
        level: 0.4,
        disabled: false,
        hint: null,
        muted: false,
      },
      {
        key: "bob@waddle.test/desktop:screen_share_audio",
        label: "bob's screen",
        source: "screen_share_audio",
        level: 0.7,
        disabled: false,
        hint: null,
        muted: false,
      },
    ]);
  });

  test("dedupes normalized participant snapshots against differently-cased LiveKit track identities", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["bob@waddle.test/Desktop"],
      remoteTracks: [
        remoteAudio("bob-mic", "Bob@Waddle.Test/Desktop", "microphone"),
        remoteAudio("bob-screen", "Bob@Waddle.Test/Desktop", "screen_share_audio"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/Desktop:microphone": 0.25,
        "bob@waddle.test/Desktop:screen_share_audio": 0.5,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      participantIdentity: row.participantIdentity,
      label: row.label,
      source: row.source,
      level: row.level,
      disabled: row.disabled,
    }))).toEqual([
      {
        key: "bob@waddle.test/Desktop:microphone",
        participantIdentity: "Bob@Waddle.Test/Desktop",
        label: "Bob",
        source: "microphone",
        level: 0.25,
        disabled: false,
      },
      {
        key: "bob@waddle.test/Desktop:screen_share_audio",
        participantIdentity: "Bob@Waddle.Test/Desktop",
        label: "Bob's screen",
        source: "screen_share_audio",
        level: 0.5,
        disabled: false,
      },
    ]);
  });

  test("keeps remembered mic level visible while a differently-cased LiveKit identity is muted", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["bob@waddle.test/Desktop"],
      remoteTracks: [],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/Desktop:microphone": 0.25,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      level: row.level,
      disabled: row.disabled,
      hint: row.hint,
      ariaValueText: row.ariaValueText,
    }))).toEqual([
      {
        key: "bob@waddle.test/Desktop:microphone",
        level: 0.25,
        disabled: true,
        hint: "mic off",
        ariaValueText: "25%",
      },
    ]);
  });

  test("normalizes remembered levels to the 0-200 percent gain range", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["alice@waddle.test/web", "bob@waddle.test/desktop"],
      remoteTracks: [
        remoteAudio("alice-mic", "alice@waddle.test/web", "microphone"),
        remoteAudio("bob-mic", "bob@waddle.test/desktop", "microphone"),
        remoteAudio("bob-screen", "bob@waddle.test/desktop", "screen_share_audio"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "alice@waddle.test/web:microphone": 3,
        "bob@waddle.test/desktop:microphone": Number.NaN,
        "bob@waddle.test/desktop:screen_share_audio": -0.5,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      level: row.level,
      ariaValueText: row.ariaValueText,
    }))).toEqual([
      {
        key: "alice@waddle.test/web:microphone",
        level: 2,
        ariaValueText: "200%",
      },
      {
        key: "bob@waddle.test/desktop:microphone",
        level: 1,
        ariaValueText: "100%",
      },
      {
        key: "bob@waddle.test/desktop:screen_share_audio",
        level: 0,
        ariaValueText: "0%",
      },
    ]);
  });
});

describe("call volume mixer reducer", () => {
  test("maps slider percentages to gain with a 100 percent snap detent", () => {
    expect(callVolumePercentToGain(0)).toBe(0);
    expect(callVolumePercentToGain(88)).toBe(0.88);
    expect(callVolumePercentToGain(99, 0.5)).toBe(0.99);
    expect(callVolumePercentToGain(99, 0.98)).toBe(1);
    expect(callVolumePercentToGain(99, 1)).toBe(0.99);
    expect(callVolumePercentToGain(100, 0.99)).toBe(1);
    expect(callVolumePercentToGain(101, 1)).toBe(1.01);
    expect(callVolumePercentToGain(101, 1.02)).toBe(1);
    expect(callVolumePercentToGain(101, 1.5)).toBe(1.01);
    expect(callVolumePercentToGain(150)).toBe(1.5);
    expect(callVolumePercentToGain(250)).toBe(2);
    expect(callVolumePercentToGain(Number.NaN)).toBe(1);
  });

  test("reset all returns every stored participant audio entry to full volume", () => {
    const levels: CallVolumeLevelStore = {
      "alice@waddle.test/web:microphone": 0.2,
      "alice@waddle.test/web:screen_share_audio": 0,
      "bob@waddle.test/desktop:microphone": 0.8,
    };

    expect(resetCallVolumeMixerLevels(levels)).toEqual({
      "alice@waddle.test/web:microphone": 1,
      "alice@waddle.test/web:screen_share_audio": 1,
      "bob@waddle.test/desktop:microphone": 1,
    });
  });
});

describe("call volume mixer controller apply path", () => {
  const callJoin = {
    url: "wss://livekit.test",
    room: "bob@waddle.test::c1",
    identity: "me@waddle.test/browser",
    token: "jwt",
  };

  afterEach(() => {
    // Idle resets the module-scoped remembered levels via the
    // controller's own SID-change subscription, isolating each test.
    clearCallState();
  });

  test("setVolume applies the chosen gain to the engine and resetAll returns it to unity", () => {
    const captured = captureEngineVolumeCalls();
    const scope = effectScope();
    try {
      activateDmCall("c1");
      let controller!: ReturnType<typeof useCallVolumeMixer>;
      scope.run(() => {
        controller = useCallVolumeMixer(computed(() => ""));
      });

      controller.setVolume(
        mixerRow({
          key: "bob@waddle.test/desktop:microphone",
          participantIdentity: "bob@waddle.test/desktop",
          source: "microphone",
        }),
        0.5,
      );
      expect(captured.calls).toContainEqual({
        participantIdentity: "bob@waddle.test/desktop",
        source: "microphone",
        volume: 0.5,
      });

      captured.calls.length = 0;
      controller.resetAll();
      // bob is both a remembered target and a projected DM row, but
      // reset-all must touch the engine once per participant, not twice.
      expect(captured.calls).toEqual([
        {
          participantIdentity: "bob@waddle.test/desktop",
          source: "microphone",
          volume: 1,
        },
      ]);
    } finally {
      scope.stop();
      captured.restore();
    }
  });

  test("remembered gain is shared across surfaces and wiped when the call SID changes", () => {
    const captured = captureEngineVolumeCalls();
    const splitScope = effectScope();
    const expandedScope = effectScope();
    const bob = mixerRow({
      key: "bob@waddle.test/desktop:microphone",
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
    });
    try {
      // A MUC call with no live participants projects no rows, so
      // reset-all touches the engine only via the remembered targets —
      // isolating the shared cross-surface state from row projection.
      activateMucCall("c1");
      let split!: ReturnType<typeof useCallVolumeMixer>;
      let expanded!: ReturnType<typeof useCallVolumeMixer>;
      splitScope.run(() => {
        split = useCallVolumeMixer(computed(() => "room@muc.waddle.test"));
      });
      expandedScope.run(() => {
        expanded = useCallVolumeMixer(computed(() => "room@muc.waddle.test"));
      });

      // A gain set from the split surface is remembered in shared state,
      // so reset-all from the EXPANDED surface returns that same target
      // to unity — one source of truth across both surfaces.
      split.setVolume(bob, 0.5);
      captured.calls.length = 0;
      expanded.resetAll();
      expect(captured.calls).toContainEqual({
        participantIdentity: "bob@waddle.test/desktop",
        source: "microphone",
        volume: 1,
      });

      // Switching to a different call forgets the remembered target:
      // reset-all from either surface now touches nothing.
      split.setVolume(bob, 0.5);
      activateMucCall("c2");
      captured.calls.length = 0;
      expanded.resetAll();
      expect(captured.calls).toEqual([]);
    } finally {
      splitScope.stop();
      expandedScope.stop();
      captured.restore();
    }
  });

  function activateDmCall(sid: string): void {
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid,
      media: { audio: true, video: false },
      join: callJoin,
      kind: "dm",
    });
  }

  function activateMucCall(sid: string): void {
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid,
      media: { audio: true, video: false },
      join: { ...callJoin, room: "room@muc.waddle.test" },
      kind: "muc",
      selfNick: "me",
      selfFullJid: "me@waddle.test/browser",
    });
  }
});

function remoteAudio(
  publicationSid: string,
  participantIdentity: string,
  source: "microphone" | "screen_share_audio",
) {
  return {
    participantIdentity,
    publicationSid,
    kind: "audio" as const,
    source,
    track: fakeTrack,
  };
}

function captureEngineVolumeCalls() {
  const { engine } = useCallEngine();
  const calls: Array<{ participantIdentity: string; source: string; volume: number }> = [];
  const target = engine as unknown as {
    setParticipantAudioVolume: (input: {
      participantIdentity: string;
      source: string;
      volume: number;
    }) => void;
  };
  const original = target.setParticipantAudioVolume;
  target.setParticipantAudioVolume = (input) => {
    calls.push(input);
  };
  return {
    calls,
    restore() {
      target.setParticipantAudioVolume = original;
    },
  };
}

function mixerRow(overrides: Partial<CallVolumeMixerRow>): CallVolumeMixerRow {
  return {
    key: "alice@waddle.test/web:microphone",
    participantIdentity: "alice@waddle.test/web",
    source: "microphone",
    label: "alice",
    level: 1,
    disabled: false,
    hint: null,
    muted: false,
    ariaLabel: "Volume for alice",
    ariaValueText: "100%",
    ...overrides,
  };
}
