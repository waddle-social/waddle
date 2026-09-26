// Receive path for the XEP-0422 `urn:waddle:safety-scores:1` fastening:
// wasm bridge shape -> typed model, with the sender gate XEP-0422
// delegates to the payload spec (room bare JID for MUC, own server for DM).

import { describe, expect, test } from "bun:test";
import { safetyScoresFasteningFromWasm } from "../src/lib/safety-scores/decode";
import {
  isTrustedDmSafetyScoresSender,
  isTrustedRoomSafetyScoresSender,
} from "../src/lib/safety-scores/sender";
import {
  dmMessageFromArchived,
  dmSafetyScoresFromWasm,
  roomMessageFromArchived,
} from "../src/lib/xmpp/wasm-message-codecs";
import type { WasmArchivedMessage, WasmSafetyScoresFastening } from "../src/lib/xmpp/wasm-types";

const ROOM_JID = "general@conference.example.org";
const SELF_BARE = "alice@example.org";
const MODEL = "typesafe/jev-1.13-20260917";

/** The agreed wire contract, as the wasm bridge serializes it. */
const contractFastening: WasmSafetyScoresFastening = {
  target_origin_id: "judged-origin-id",
  target_stanza_id: "judged-stanza-id",
  target_stanza_by: ROOM_JID,
  source_revision_id: "judged-stanza-id",
  scores: {
    model_version: MODEL,
    scores: [
      { category: "is_question", probability: 0.92, taxonomy_version: "is-question-v1" },
      { category: "safety:hate_speech", probability: 0.03, taxonomy_version: "safety-hate-speech-v1" },
      { category: "safety:explicit", probability: 0.01, taxonomy_version: "safety-explicit-v1" },
      { category: "safety:harassment", probability: 0.02, taxonomy_version: "safety-harassment-v1" },
      { category: "safety:violence", probability: 0, taxonomy_version: "safety-violence-v1" },
      { category: "safety:self_harm", probability: 0, taxonomy_version: "safety-self-harm-v1" },
    ],
  },
};

function archivedRoom(overrides: Partial<WasmArchivedMessage> = {}): WasmArchivedMessage {
  return {
    mam_id: "mam-scores-1",
    id: "fastening-1",
    message_type: "groupchat",
    from: ROOM_JID,
    to: `${SELF_BARE}/web`,
    timestamp: "2026-09-25T10:00:00Z",
    reaction_emojis: [],
    is_muc: true,
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
    link_previews: [],
    safety_scores: contractFastening,
    ...overrides,
  } as WasmArchivedMessage;
}

describe("safetyScoresFasteningFromWasm", () => {
  test("decodes the contract fixture into typed scores", () => {
    expect(safetyScoresFasteningFromWasm(contractFastening)).toEqual({
      targetOriginId: "judged-origin-id",
      targetStanzaId: "judged-stanza-id",
      targetStanzaBy: ROOM_JID,
      sourceRevisionId: "judged-stanza-id",
      scores: {
        modelVersion: MODEL,
        scores: [
          { category: "is_question", probability: 0.92, taxonomyVersion: "is-question-v1" },
          { category: "safety:hate_speech", probability: 0.03, taxonomyVersion: "safety-hate-speech-v1" },
          { category: "safety:explicit", probability: 0.01, taxonomyVersion: "safety-explicit-v1" },
          { category: "safety:harassment", probability: 0.02, taxonomyVersion: "safety-harassment-v1" },
          { category: "safety:violence", probability: 0, taxonomyVersion: "safety-violence-v1" },
          { category: "safety:self_harm", probability: 0, taxonomyVersion: "safety-self-harm-v1" },
        ],
      },
    });
  });

  test("skips unknown categories and out-of-range scores without dropping the batch", () => {
    const decoded = safetyScoresFasteningFromWasm({
      ...contractFastening,
      scores: {
        model_version: MODEL,
        scores: [
          { category: "safety:spam", probability: 0.4, taxonomy_version: "safety-spam-v1" },
          { category: "safety:violence", probability: 1.2, taxonomy_version: "v" },
          { category: "safety:explicit", probability: Number.NaN, taxonomy_version: "v" },
          { category: "safety:harassment", probability: 0.5, taxonomy_version: "" },
          { category: "is_question", probability: 0.7, taxonomy_version: "is-question-v1" },
          { category: "is_question", probability: 0.1, taxonomy_version: "is-question-v2" },
        ],
      },
    });
    expect(decoded).toEqual({
      targetOriginId: "judged-origin-id",
      targetStanzaId: "judged-stanza-id",
      targetStanzaBy: ROOM_JID,
      sourceRevisionId: "judged-stanza-id",
      scores: {
        modelVersion: MODEL,
        scores: [{ category: "is_question", probability: 0.7, taxonomyVersion: "is-question-v1" }],
      },
    });
  });

  test("rejects a fastening with no target or no model version", () => {
    expect(safetyScoresFasteningFromWasm({ ...contractFastening, target_origin_id: "" })).toBeNull();
    expect(safetyScoresFasteningFromWasm({
      ...contractFastening,
      scores: { model_version: "", scores: [] },
    })).toBeNull();
  });
});

describe("safety-scores sender gate", () => {
  test("MUC: only the room bare JID may fasten scores", () => {
    expect(isTrustedRoomSafetyScoresSender(ROOM_JID, ROOM_JID)).toBe(true);
    expect(isTrustedRoomSafetyScoresSender("General@Conference.Example.org", ROOM_JID)).toBe(true);
    expect(isTrustedRoomSafetyScoresSender(`${ROOM_JID}/mallory`, ROOM_JID)).toBe(false);
    expect(isTrustedRoomSafetyScoresSender("other@conference.example.org", ROOM_JID)).toBe(false);
    expect(isTrustedRoomSafetyScoresSender(ROOM_JID, "")).toBe(false);
  });

  test("DM: only the account's own server domain may fasten scores", () => {
    expect(isTrustedDmSafetyScoresSender("example.org", SELF_BARE)).toBe(true);
    expect(isTrustedDmSafetyScoresSender("Example.org", SELF_BARE)).toBe(true);
    expect(isTrustedDmSafetyScoresSender("bob@example.org", SELF_BARE)).toBe(false);
    expect(isTrustedDmSafetyScoresSender("example.org/resource", SELF_BARE)).toBe(false);
    expect(isTrustedDmSafetyScoresSender("evil.example", SELF_BARE)).toBe(false);
    expect(isTrustedDmSafetyScoresSender(SELF_BARE, SELF_BARE)).toBe(false);
  });
});

describe("roomMessageFromArchived with a safety-scores fastening", () => {
  test("a room-authored fastening decodes to a non-rendering record", () => {
    const decoded = roomMessageFromArchived(archivedRoom());
    expect(decoded).toMatchObject({
      roomJid: ROOM_JID,
      body: "",
      type: "message",
      safetyScoresFastening: { targetOriginId: "judged-origin-id", targetStanzaId: "judged-stanza-id" },
    });
    expect(decoded?.safetyScoresFastening?.scores.scores).toHaveLength(6);
  });

  test("the live path decodes the same record", () => {
    const decoded = roomMessageFromArchived(archivedRoom(), "live");
    expect(decoded?.safetyScoresFastening?.targetStanzaId).toBe("judged-stanza-id");
  });

  test("an occupant-sent fastening is dropped entirely", () => {
    expect(roomMessageFromArchived(archivedRoom({ from: `${ROOM_JID}/mallory` }))).toBeNull();
  });

  test("an occupant-sent fastening with a fallback body is still dropped", () => {
    expect(roomMessageFromArchived(archivedRoom({ from: `${ROOM_JID}/mallory`, body: "scores" })))
      .toBeNull();
  });
});

describe("DM safety-scores fastening", () => {
  const dmFastening = {
    from: "example.org",
    safety_scores: { ...contractFastening, target_stanza_id: "dm-stanza-id" },
  };

  test("accepted from the account's own server", () => {
    expect(dmSafetyScoresFromWasm(dmFastening, SELF_BARE)?.targetStanzaId).toBe("dm-stanza-id");
  });

  test("rejected from the DM peer", () => {
    expect(dmSafetyScoresFromWasm({ ...dmFastening, from: "bob@example.org/phone" }, SELF_BARE))
      .toBeNull();
  });

  test("never materialises as a DM timeline row", () => {
    const archived = archivedRoom({
      message_type: "chat",
      is_muc: false,
      from: "bob@example.org/phone",
      body: "fallback text",
    });
    expect(dmMessageFromArchived(archived, SELF_BARE)).toBeNull();
  });
});
