import { describe, expect, test } from "bun:test";
import {
  dmMessageFromArchived,
  roomMessageFromArchived,
} from "@/lib/xmpp/client";
import type { WasmArchivedMessage } from "@/lib/xmpp/wasm-types";

const baseArchivedRoom: WasmArchivedMessage = {
  mam_id: "mam-1",
  message_type: "groupchat",
  from: "room@conf.example.com/alice",
  to: "bob@example.com",
  body: "see https://example.com",
  reaction_emojis: [],
  is_muc: true,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

const baseArchivedDm: WasmArchivedMessage = {
  mam_id: "mam-dm-1",
  message_type: "chat",
  from: "alice@example.com/web",
  to: "bob@example.com",
  body: "see https://example.com",
  reaction_emojis: [],
  is_muc: false,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

describe("roomMessageFromArchived", () => {
  test("maps XEP-0372 references with anchor onto LiveRoomMessage.references", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      references: [
        {
          ref_type: "data",
          uri: "https://example.com",
          begin: 4,
          end: 23,
          anchor: "https://example.com",
        },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result).not.toBeNull();
    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "https://example.com",
        begin: 4,
        end: 23,
        anchor: "https://example.com",
      },
    ]);
  });

  test("omits anchor when absent", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      references: [
        { ref_type: "mention", uri: "xmpp:bob@example.com", begin: 0, end: 4 },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.references).toEqual([
      { type: "mention", uri: "xmpp:bob@example.com", begin: 0, end: 4 },
    ]);
  });

  test("leaves references undefined when WASM payload has none", () => {
    const result = roomMessageFromArchived(baseArchivedRoom);
    expect(result?.references).toBeUndefined();
  });
});

describe("roomMessageFromArchived with reply-fallback", () => {
  test("preserves anchor-only (0, 0) reference intact when stripping fallback", () => {
    // Inbound: external client sent an anchor-only reference (no body
    // position, omitted begin/end → parsed as (0, 0)) on a reply that has a
    // fallback prefix. stripReplyFallback must NOT delete the reference.
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      body: "> quoted\n\nactual reply",
      reply_fallback_start: 0,
      reply_fallback_end: 10,
      references: [
        {
          ref_type: "data",
          uri: "xmpp:room@conf.example?message;id=earlier",
          begin: 0,
          end: 0,
          anchor: "xmpp:alice@example.com",
        },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "xmpp:room@conf.example?message;id=earlier",
        begin: 0,
        end: 0,
        anchor: "xmpp:alice@example.com",
      },
    ]);
  });
});

describe("dmMessageFromArchived", () => {
  test("maps XEP-0372 references onto LiveDmMessage.references", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedDm,
      references: [
        {
          ref_type: "data",
          uri: "https://example.com",
          begin: 4,
          end: 23,
        },
      ],
    };

    const result = dmMessageFromArchived(archived, "bob@example.com");

    expect(result).not.toBeNull();
    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "https://example.com",
        begin: 4,
        end: 23,
      },
    ]);
  });
});
